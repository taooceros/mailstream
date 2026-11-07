import 'dart:collection';
import 'dart:isolate';

import 'package:html/dom.dart' as dom;
import 'package:html/parser.dart' as html_parser;

typedef HtmlParts = (String main, String? quoted);

/// Only strings cross the isolate boundary; native views and channels stay in the reader.
Future<HtmlParts> splitMailHtmlInBackground(String html) =>
    Isolate.run(() => splitMailHtml(html), debugName: 'mail-html');

/// The body's original quote policy, shared with the runtime comparison harness.
HtmlParts splitMailHtml(String html) {
  final document = html_parser.parse(html);
  final body = document.body;
  if (body == null) return (html, null);
  final quotes = <dom.Element>[];
  void collect(dom.Element element) {
    for (final child in element.children) {
      if (child.classes.contains('gmail_quote') ||
          child.classes.contains('gmail_quote_container') ||
          child.classes.contains('moz-cite-prefix') ||
          child.id == 'divRplyFwdMsg' ||
          child.id == 'appendonsend' ||
          (child.localName == 'blockquote' && child.attributes['type'] == 'cite')) {
        quotes.add(child);
      } else {
        collect(child);
      }
    }
  }

  collect(body);
  if (quotes.isEmpty) return (html, null);
  final quoted = quotes.map((q) => q.outerHtml).join();
  for (final quote in quotes) {
    quote.remove();
  }
  final kept = body.text.trim().isNotEmpty || body.querySelector('img') != null;
  if (!kept) return (html, null);
  final head = document.head?.outerHtml ?? '';
  return (document.outerHtml, '<html>$head<body>$quoted</body></html>');
}

/// The text of an HTML body, read in a background isolate, for a reader without an HTML view.
Future<String> htmlTextInBackground(String html) => Isolate.run(() => mailHtmlText(html), debugName: 'mail-html-text');

/// Block elements end their line in [mailHtmlText].
const _blocks = 'p, div, tr, li, h1, h2, h3, h4, h5, h6, table, blockquote, pre, ul, ol, section, article, header, footer';

/// [html]'s visible text: no scripts, styles or head; line breaks and blocks end lines.
String mailHtmlText(String html) {
  final document = html_parser.parse(html);
  final body = document.body;
  if (body == null) return '';
  for (final hidden in body.querySelectorAll('script, style, template')) {
    hidden.remove();
  }
  for (final br in body.querySelectorAll('br')) {
    br.replaceWith(dom.Text('\n'));
  }
  for (final block in body.querySelectorAll(_blocks)) {
    block.append(dom.Text('\n'));
  }
  return body.text;
}

/// Reopening/rebuilding unchanged mail reuses its parts, including in-flight work.
/// One active isolate bounds CPU pressure when a thread contains many HTML messages.
class HtmlPartsCache {
  HtmlPartsCache({Future<HtmlParts> Function(String) split = splitMailHtmlInBackground}) : _split = split;

  static final shared = HtmlPartsCache();
  static const capacity = 8;
  final Future<HtmlParts> Function(String) _split;
  final _ready = LinkedHashMap<String, HtmlParts>();
  final _pending = <String, Future<HtmlParts>>{};
  Future<void> _tail = Future.value();

  HtmlParts? peek(String html) {
    final parts = _ready.remove(html);
    if (parts != null) _ready[html] = parts;
    return parts;
  }

  Future<HtmlParts> load(String html) {
    final ready = peek(html);
    if (ready != null) return Future.value(ready);
    return _pending.putIfAbsent(html, () {
      final work = _tail.then((_) => _split(html)).then((parts) {
        _ready[html] = parts;
        if (_ready.length > capacity) _ready.remove(_ready.keys.first);
        return parts;
      }).whenComplete(() {
        _pending.remove(html);
      });
      // A failed parse must not stop the queue or become a cached failure.
      _tail = work.then<void>((_) {}, onError: (Object _, StackTrace __) {});
      return work;
    });
  }
}
