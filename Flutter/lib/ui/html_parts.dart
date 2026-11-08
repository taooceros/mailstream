import 'dart:async';
import 'dart:collection';
import 'dart:isolate';

import 'package:html/dom.dart' as dom;
import 'package:html/parser.dart' as html_parser;

import '../platform.dart';
import 'hyper_resources.dart';

typedef HtmlParts = (String main, String? quoted);

/// Only strings cross the isolate boundary; native views and channels stay in the reader.
Future<HtmlParts> splitMailHtmlInBackground(String html) =>
    Isolate.run(() => splitMailHtml(html), debugName: 'mail-html');

/// The Mac reader's preparation, shared by both its renderers: inline cache images become data
/// URLs, unsafe sources go, presentational attributes get `data-ms-*` copies, then quotes are
/// split. Windows' WebView2 bodies get the plain split.
Future<HtmlParts> prepareMailHtmlInBackground(String html) =>
    Isolate.run(() async => splitMailHtml(await prepareHyperResources(html)), debugName: 'mail-html');

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

/// [html] as the page a WebView2 body loads on Windows (`WindowsMailHtml`): links open in the
/// same page, where the reader's navigation handler sends them to the browser. WebView2 opens
/// `target="_blank"` links and forms in a window of its own, which `webview_plus` leaves to it.
String windowsMailPage(String html) {
  final document = html_parser.parse(html);
  for (final element in document.querySelectorAll('[target]')) {
    element.attributes.remove('target');
  }
  return document.outerHtml;
}

/// Reopening/rebuilding unchanged mail reuses its parts, including in-flight work.
/// One active isolate bounds CPU pressure when a thread contains many HTML messages.
///
/// Readers' requests go first, in order; a request every waiter released before its turn is
/// dropped without splitting (a running split finishes). Prefetch work ([warm]) runs only when
/// no reader waits, a newer batch replaces the pending one, and its results wait in an LRU of
/// their own, so they never push a reader's parts out.
class HtmlPartsCache {
  HtmlPartsCache({this._split = splitMailHtmlInBackground});

  static final shared = HtmlPartsCache(split: Host.mac ? prepareMailHtmlInBackground : splitMailHtmlInBackground);
  static const capacity = 8;
  final Future<HtmlParts> Function(String) _split;
  final _ready = <String, HtmlParts>{};
  final _warmed = <String, HtmlParts>{};

  /// Queued and running work by HTML; [_active] and [_warm] hold the queued jobs in order.
  final _jobs = <String, _Job>{};
  final _active = LinkedHashSet<_Job>.identity();
  final _warm = LinkedHashSet<_Job>.identity();
  _Job? _running;

  /// Parts ready now; prefetched ones move to the reader's LRU as they are taken.
  HtmlParts? peek(String html) {
    final parts = _ready.remove(html) ?? _warmed.remove(html);
    if (parts != null) _keep(_ready, html, parts);
    return parts;
  }

  /// Parts for a reader, ahead of every prefetch. Release the request when its parts are no
  /// longer wanted: work no request waits for any more is skipped before it starts.
  HtmlPartsRequest request(String html) {
    final ready = peek(html);
    if (ready != null) return HtmlPartsRequest._(null, Future.value(ready));
    final job = _jobs.putIfAbsent(html, () => _Job(html));
    if (!job.started) {
      // Queued prefetch work a reader wants moves into the readers' queue.
      _warm.remove(job);
      _active.add(job);
    }
    job
      ..warm = false
      ..waiters += 1;
    _pump();
    return HtmlPartsRequest._(this, job.done.future, job);
  }

  /// [request]'s parts, for a caller that keeps waiting for them.
  Future<HtmlParts> load(String html) => request(html).parts;

  /// Splits [htmls] when no reader waits (a prefetched conversation's opening bodies, most
  /// wanted first), in place of the prefetch work still queued.
  void warm(Iterable<String> htmls) {
    for (final job in _warm) {
      _jobs.remove(job.html);
    }
    _warm.clear();
    for (final html in htmls.take(capacity)) {
      if (_ready.containsKey(html) || _warmed.containsKey(html) || _jobs.containsKey(html)) continue;
      _warm.add(_jobs[html] = _Job(html)..warm = true);
    }
    _pump();
  }

  void _release(_Job job) {
    job.waiters -= 1;
    if (job.waiters > 0 || job.started) return;
    _active.remove(job);
    _jobs.remove(job.html);
  }

  void _pump() {
    if (_running != null) return;
    final queue = _active.isNotEmpty ? _active : _warm;
    if (queue.isEmpty) return;
    final job = _running = queue.first;
    queue.remove(job);
    job.started = true;
    Future.sync(() => _split(job.html)).then((parts) {
      _keep(job.warm ? _warmed : _ready, job.html, parts);
      job.done.complete(parts);
    }, onError: job.done.completeError).whenComplete(() {
      // A failed parse must not stop the queue or become a cached failure.
      _jobs.remove(job.html);
      _running = null;
      _pump();
    });
  }

  /// Map literals keep insertion order, so the first key is the least recently used.
  static void _keep(Map<String, HtmlParts> lru, String html, HtmlParts parts) {
    lru[html] = parts;
    if (lru.length > capacity) lru.remove(lru.keys.first);
  }
}

/// A reader's claim on the parts of one HTML body.
class HtmlPartsRequest {
  HtmlPartsRequest._(this._cache, this.parts, [this._job]);

  final HtmlPartsCache? _cache;
  final _Job? _job;
  bool _released = false;

  /// Completes with the parts unless every request for them is released before the split starts.
  final Future<HtmlParts> parts;

  /// No longer waits for the parts; once no request does, a split not yet started is dropped.
  void release() {
    if (_released) return;
    _released = true;
    if (_job case final job?) _cache!._release(job);
  }
}

class _Job {
  _Job(this.html) {
    // Prefetch work has no listener: its failure is dropped, as a reader's is reported by it.
    done.future.ignore();
  }

  final String html;
  final done = Completer<HtmlParts>();
  bool warm = false;
  bool started = false;
  int waiters = 0;
}
