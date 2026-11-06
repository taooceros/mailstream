import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';

import 'design.dart';

/// One block of a Chat message.
sealed class ChatBlock {
  const ChatBlock();
}

/// Lines of text; line breaks are kept.
final class ChatParagraph extends ChatBlock {
  const ChatParagraph(this.text);

  final String text;
}

/// A ```monospace block```.
final class ChatCode extends ChatBlock {
  const ChatCode(this.text);

  final String text;
}

/// A `* item` or `- item` line; four spaces before the marker per level of nesting.
final class ChatBullet extends ChatBlock {
  const ChatBullet(this.text, this.depth);

  final String text;
  final int depth;
}

/// Lines starting with `>`.
final class ChatQuote extends ChatBlock {
  const ChatQuote(this.text);

  final String text;
}

/// Google Chat's message markup, as the API's `formattedText` carries it and the Chat web app
/// draws it: ```monospace blocks```, `* `/`- ` bulleted lists (four spaces per nesting level),
/// `>` block quotes, and inline `*bold*`, `_italic_`, `~strike~`, `` `monospace` ``,
/// `<url|label>` links, `<users/…>` mentions and bare links. Everything else is plain text with
/// its line breaks. Google escapes punctuation it means literally (`\*\*not bold\*\*`, `1\.`);
/// those show without the backslash, except in monospace, which Google leaves unescaped.
abstract final class ChatMarkup {
  static final _fence = RegExp(r'```\n?([\s\S]*?)\n?```');
  static final _bullet = RegExp(r'^( *)[*-] (.*)$');
  static final _quote = RegExp(r'^> ?(.*)$');
  static final _escape = RegExp(r'\\([!-/:-@\[-`{-~])');

  /// Escaped characters stand in the private-use area from here while the markup is parsed, so
  /// no rule takes them for markup.
  static const _hidden = 0xE000;

  static String _hide(String text) =>
      text.replaceAllMapped(_escape, (m) => String.fromCharCode(_hidden + m.group(1)!.codeUnitAt(0)));

  /// The text as shown: escaped characters without their backslash, or (`literal`, monospace)
  /// with it.
  static String _show(String text, {bool literal = false}) {
    if (!text.codeUnits.any((c) => c >= _hidden && c < _hidden + 0x80)) return text;
    final out = StringBuffer();
    for (final c in text.codeUnits) {
      if (c >= _hidden && c < _hidden + 0x80) {
        if (literal) out.write(r'\');
        out.writeCharCode(c - _hidden);
      } else {
        out.writeCharCode(c);
      }
    }
    return out.toString();
  }

  /// Blocks whose text still carries escapes hidden; `inline` shows them.
  static List<ChatBlock> blocks(String text) {
    final hidden = _hide(text);
    final out = <ChatBlock>[];
    var cursor = 0;
    for (final match in _fence.allMatches(hidden)) {
      _lines(hidden.substring(cursor, match.start), out);
      out.add(ChatCode(_show(match.group(1)!, literal: true)));
      cursor = match.end;
    }
    _lines(hidden.substring(cursor), out);
    return out;
  }

  /// Splits text outside monospace blocks into paragraphs, bullets and quotes.
  static void _lines(String text, List<ChatBlock> out) {
    // The run of plain or quoted lines being gathered.
    final run = <String>[];
    var quoting = false;
    void flush() {
      if (quoting) {
        out.add(ChatQuote(run.join('\n')));
      } else {
        // Blank lines at a paragraph's edges belong to the gap between blocks.
        final first = run.indexWhere((l) => l.trim().isNotEmpty);
        final last = run.lastIndexWhere((l) => l.trim().isNotEmpty);
        if (first >= 0) out.add(ChatParagraph(run.sublist(first, last + 1).join('\n')));
      }
      run.clear();
    }

    for (final line in text.split('\n')) {
      if (_bullet.firstMatch(line) case final bullet?) {
        if (run.isNotEmpty) flush();
        out.add(ChatBullet(bullet.group(2)!, bullet.group(1)!.length ~/ 4));
        continue;
      }
      final quoted = _quote.firstMatch(line);
      if (run.isNotEmpty && quoting != (quoted != null)) flush();
      quoting = quoted != null;
      run.add(quoted?.group(1) ?? line);
    }
    if (run.isNotEmpty) flush();
  }

  static final _code = RegExp(r'`([^`\n]+)`');
  static final _token = RegExp(r'<(https?://[^|>\s]+)(?:\|([^>\n]*))?>|<(users/[^>\s]+)>');
  static final _link = RegExp(r'''\b(?:https?://|mailto:)[^\s<>"']+''');
  static final _emphasis = RegExp(
    r'(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])|(?<![\w_])_(?=\S)([^_\n]*?\S)_(?![\w_])|(?<![\w~])~(?=\S)([^~\n]*?\S)~(?![\w~])',
  );

  /// One block's inline markup as spans. `link` makes a span open a URL; `names` maps
  /// `users/<id>` to the names mentions show.
  static List<InlineSpan> inline(
    String text, {
    required TextStyle code,
    required TextStyle link,
    required TextStyle mention,
    required Map<String, String> names,
    required GestureRecognizer Function(String url) open,
  }) {
    final out = <InlineSpan>[];
    _split(text, _code, (match) {
      out.add(TextSpan(text: _show(match.group(1)!, literal: true), style: code));
    }, (part) {
      _split(part, _token, (match) {
        if (match.group(1) case final hiddenUrl?) {
          final url = _show(hiddenUrl);
          final label = _show(match.group(2) ?? '');
          out.add(TextSpan(text: label.isEmpty ? url : label, style: link, recognizer: open(url)));
        } else {
          out.add(TextSpan(text: '@${names[match.group(3)] ?? 'user'}', style: mention));
        }
      }, (part) {
        _split(part, _link, (match) {
          var url = match.group(0)!;
          // Sentence punctuation after a link is not part of it.
          final trailing = RegExp(r'''[.,;:!?)\]'"]+$''').firstMatch(url)?.group(0) ?? '';
          url = _show(url.substring(0, url.length - trailing.length));
          out.add(TextSpan(text: url, style: link, recognizer: open(url)));
          if (trailing.isNotEmpty) out.addAll(_emphasized(trailing));
        }, (part) => out.addAll(_emphasized(part)));
      });
    });
    return out;
  }

  /// `*bold*`, `_italic_` and `~strike~`, nested in any order.
  static List<InlineSpan> _emphasized(String text) {
    final out = <InlineSpan>[];
    _split(text, _emphasis, (match) {
      final TextStyle added;
      final String inner;
      if (match.group(1) != null) {
        added = const TextStyle(fontWeight: FontWeight.w700);
        inner = match.group(1)!;
      } else if (match.group(2) != null) {
        added = const TextStyle(fontStyle: FontStyle.italic);
        inner = match.group(2)!;
      } else {
        added = const TextStyle(decoration: TextDecoration.lineThrough);
        inner = match.group(3)!;
      }
      out.add(TextSpan(style: added, children: _emphasized(inner)));
    }, (part) => out.add(TextSpan(text: _show(part))));
    return out;
  }

  static void _split(String text, RegExp regex, void Function(RegExpMatch) match, void Function(String) rest) {
    var cursor = 0;
    for (final found in regex.allMatches(text)) {
      if (found.start > cursor) rest(text.substring(cursor, found.start));
      match(found);
      cursor = found.end;
    }
    if (cursor < text.length) rest(text.substring(cursor));
  }
}

/// A Chat message's text, drawn as Google Chat draws its markup. Selectable across blocks;
/// links open in the browser through `onOpen`.
class ChatText extends StatefulWidget {
  const ChatText({super.key, required this.text, required this.names, required this.onOpen});

  final String text;

  /// `users/<id>` → name, for mentions.
  final Map<String, String> names;
  final void Function(String url) onOpen;

  @override
  State<ChatText> createState() => _ChatTextState();
}

class _ChatTextState extends State<ChatText> {
  final List<TapGestureRecognizer> _recognizers = [];

  @override
  void dispose() {
    _disposeRecognizers();
    super.dispose();
  }

  void _disposeRecognizers() {
    for (final recognizer in _recognizers) {
      recognizer.dispose();
    }
    _recognizers.clear();
  }

  GestureRecognizer _open(String url) {
    final recognizer = TapGestureRecognizer()..onTap = () => widget.onOpen(url);
    _recognizers.add(recognizer);
    return recognizer;
  }

  @override
  Widget build(BuildContext context) {
    _disposeRecognizers();
    final colors = context.colors;
    final body = context.text.body;
    final mono = body.copyWith(fontFamily: 'Menlo', fontSize: (body.fontSize ?? 13) * 0.92);
    final code = mono.copyWith(backgroundColor: colors.strongFill, color: colors.label);
    final link = TextStyle(color: colors.accent, decoration: TextDecoration.underline, decorationColor: colors.accent);
    final mention = TextStyle(color: colors.accent, fontWeight: FontWeight.w600);
    List<InlineSpan> spans(String text) =>
        ChatMarkup.inline(text, code: code, link: link, mention: mention, names: widget.names, open: _open);

    final children = <Widget>[];
    for (final block in ChatMarkup.blocks(widget.text)) {
      if (children.isNotEmpty) children.add(const SizedBox(height: 4));
      children.add(switch (block) {
        ChatParagraph(:final text) => Text.rich(TextSpan(children: spans(text)), style: body),
        ChatCode(:final text) => Container(
          width: double.infinity,
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
          decoration: BoxDecoration(
            color: colors.fill,
            border: Border.all(color: colors.separator),
            borderRadius: BorderRadius.circular(6),
          ),
          child: Text(text, style: mono),
        ),
        ChatBullet(:final text, :final depth) => Padding(
          padding: EdgeInsets.only(left: 4 + depth * 20.0),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SizedBox(width: 16, child: Text(depth.isEven ? '•' : '◦', style: body)),
              Expanded(child: Text.rich(TextSpan(children: spans(text)), style: body)),
            ],
          ),
        ),
        ChatQuote(:final text) => Container(
          padding: const EdgeInsets.only(left: 10),
          decoration: BoxDecoration(border: Border(left: BorderSide(color: colors.border, width: 3))),
          child: Text.rich(TextSpan(children: spans(text)), style: body.copyWith(color: colors.secondaryLabel)),
        ),
      });
    }
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: children);
  }
}
