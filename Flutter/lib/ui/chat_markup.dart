import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:mailstream/mailstream.dart' as engine;

import 'design.dart';

/// The type of a message's text: a size up from the list's and looser lines, for reading.
TextStyle chatReading(BuildContext context) => context.text.body.copyWith(fontSize: 14, height: 1.5, letterSpacing: 0);

/// A Chat message's text as the engine lays it out (`ChatMessage.body`, from the Markdown Google
/// sends; MS-235): blocks one below the other, each in its quotes and lists, with styled runs.
/// Paragraphs flow to the column's width, quotes take a bar per level, list items their marker,
/// code blocks a monospace box and tables a grid; links open in the browser through `onOpen`.
class ChatText extends StatefulWidget {
  const ChatText({super.key, required this.blocks, required this.onOpen});

  final List<engine.ChatBlock> blocks;
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

  @override
  Widget build(BuildContext context) {
    _disposeRecognizers();
    final colors = context.colors;
    final body = chatReading(context);
    final mono = TextStyle(fontFamily: MailFonts.mono, fontSize: (body.fontSize ?? 14) * 0.86, height: 1.5, color: colors.label);
    final renderer = _Renderer(
      colors: colors,
      body: body,
      mono: mono,
      code: mono.copyWith(backgroundColor: colors.fill),
      link: TextStyle(color: colors.accent),
      mention: TextStyle(color: colors.accent, fontWeight: FontWeight.w600),
      open: (url) {
        final recognizer = TapGestureRecognizer()..onTap = () => widget.onOpen(url);
        _recognizers.add(recognizer);
        return recognizer;
      },
    );
    return renderer.column(widget.blocks);
  }
}

/// Lays the engine's blocks out as widgets and their runs as spans.
class _Renderer {
  _Renderer({
    required this.colors,
    required this.body,
    required this.mono,
    required this.code,
    required this.link,
    required this.mention,
    required this.open,
  });

  final MailColors colors;
  final TextStyle body, mono, code, link, mention;
  final GestureRecognizer Function(String url) open;

  /// Between paragraphs and other blocks: half a line; between the items of a tight list, less.
  static const _blockGap = 8.0;
  static const _tightGap = 2.0;

  /// The column a list item's marker sits in.
  static const _markerWidth = 22.0;

  Widget column(List<engine.ChatBlock> blocks) {
    final children = <Widget>[];
    for (final block in blocks) {
      final content = _block(block);
      if (content == null) continue;
      final gap = children.isEmpty ? 0.0 : (block.tight ? _tightGap : _blockGap);
      children.add(_placed(block, content, gap));
    }
    if (children.isEmpty) return const SizedBox.shrink();
    if (children.length == 1) return children.single;
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: children);
  }

  /// `content` in its lists (an indent each, the item's marker beside its first block) and quotes
  /// (a bar each), `gap` above it inside the bars so a quote's bar runs on between its blocks.
  Widget _placed(engine.ChatBlock block, Widget content, double gap) {
    Widget placed = content;
    final marker = block.marker;
    if (marker != null) {
      placed = Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: _markerWidth,
            child: Text(marker, style: body.copyWith(color: colors.secondaryLabel, fontFeatures: const [FontFeature.tabularFigures()])),
          ),
          Expanded(child: placed),
        ],
      );
    }
    final indent = (block.indent - (marker == null ? 0 : 1)) * _markerWidth;
    placed = Padding(padding: EdgeInsets.only(left: indent, top: gap), child: placed);
    for (var level = 0; level < block.quote; level++) {
      placed = Container(
        padding: const EdgeInsets.only(left: 12),
        decoration: BoxDecoration(border: Border(left: BorderSide(color: colors.border, width: 3))),
        child: placed,
      );
    }
    return placed;
  }

  Widget? _block(engine.ChatBlock block) {
    final style = block.quote > 0 ? body.copyWith(color: colors.secondaryLabel) : body;
    switch (block.kind) {
      case engine.HeadingChatBlockKind(:final level):
        final scale = switch (level) { 1 => 1.3, 2 => 1.18, 3 => 1.08, _ => 1.0 };
        final heading = style.copyWith(fontWeight: FontWeight.w700, fontSize: (style.fontSize ?? 14) * scale, height: 1.3);
        return Text.rich(TextSpan(children: _spans(block.spans)), style: heading);
      case engine.CodeChatBlockKind():
        return Container(
          width: double.infinity,
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          decoration: BoxDecoration(color: colors.fill, borderRadius: BorderRadius.circular(8)),
          child: Text(block.spans.map((span) => span.text).join(), style: mono.copyWith(height: 1.45)),
        );
      case engine.RuleChatBlockKind():
        return Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Container(height: 1, color: colors.separator),
        );
      case engine.TableChatBlockKind():
        return ClipRRect(
          borderRadius: BorderRadius.circular(8),
          child: Table(
            defaultColumnWidth: const IntrinsicColumnWidth(),
            border: TableBorder.symmetric(inside: BorderSide(color: colors.separator)),
            children: [
              for (final (r, row) in block.rows.indexed)
                TableRow(
                  decoration: BoxDecoration(color: r == 0 ? colors.fill : null),
                  children: [
                    for (final cell in row)
                      Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
                        child: Text.rich(
                          TextSpan(children: _spans(cell)),
                          style: r == 0 ? style.copyWith(fontWeight: FontWeight.w600) : style,
                        ),
                      ),
                  ],
                ),
            ],
          ),
        );
      default:
        if (block.spans.every((span) => span.text.trim().isEmpty)) return null;
        return Text.rich(TextSpan(children: _spans(block.spans)), style: style);
    }
  }

  List<InlineSpan> _spans(List<engine.ChatSpan> spans) => [for (final span in spans) _span(span)];

  InlineSpan _span(engine.ChatSpan span) {
    final look = span.style;
    final url = look.link;
    final recognizer = url == null ? null : open(url);
    if (look.code) {
      // Thin spaces in the code's fill pad it at both ends, like a chip.
      return TextSpan(text: '\u2009${span.text}\u2009', style: url == null ? code : code.merge(link), recognizer: recognizer);
    }
    var style = const TextStyle();
    if (look.mention) style = style.merge(mention);
    if (url != null) style = style.merge(link);
    if (look.bold) style = style.copyWith(fontWeight: FontWeight.w600);
    if (look.italic) style = style.copyWith(fontStyle: FontStyle.italic);
    if (look.strike) style = style.copyWith(decoration: TextDecoration.lineThrough);
    return TextSpan(text: span.text, style: style, recognizer: recognizer);
  }
}
