import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:markdown/markdown.dart' as md;

import 'design.dart';

/// A Chat message's `formattedText` as Markdown nodes. Google answers projects without a Chat app
/// configuration (Mailstream's) in Markdown: CommonMark with GitHub's extensions (`~~strike~~`,
/// tables, bare links), escapes for what is meant literally (`\*\*`, `1\.`), and mentions as
/// `<chat-user data-user="users/…">` or `data-email="…"`. Lines are wrapped at 80 columns, so a
/// single line break is a space; a line break the sender typed comes as a hard break (`\`).
List<md.Node> parseChatMarkdown(String text) => md.Document(
  extensionSet: md.ExtensionSet.gitHubFlavored,
  inlineSyntaxes: [_MentionSyntax()],
  encodeHtml: false,
).parse(text);

/// `<chat-user data-user="users/…"></chat-user>` (or `data-email`) as a `mention` element whose
/// `user` or `email` attribute names the person.
class _MentionSyntax extends md.InlineSyntax {
  _MentionSyntax() : super(r'<chat-user\s+data-(user|email)="([^"]*)"[^>]*>(?:\s*</chat-user>)?', startCharacter: 0x3C);

  @override
  bool onMatch(md.InlineParser parser, Match match) {
    parser.addNode(md.Element.empty('mention')..attributes[match[1]!] = match[2]!);
    return true;
  }
}

const _blockTags = {
  'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'pre', 'blockquote', 'ul', 'ol', 'li', 'hr', 'table', //
};

/// A soft line break and the indentation around it, which Markdown reads as one space.
final _softBreak = RegExp(r'[ \t]*\n[ \t]*');

/// The type of a message's text: a size up from the list's and looser lines, for reading.
TextStyle chatReading(BuildContext context) => context.text.body.copyWith(fontSize: 14, height: 1.5, letterSpacing: 0);

/// A Chat message's text, drawn from its Markdown: paragraphs flow to the column's width, lists,
/// quotes, monospace blocks and tables are laid out, links open in the browser through `onOpen`,
/// and mentions show the person's name.
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
  late List<md.Node> _nodes = parseChatMarkdown(widget.text);

  @override
  void didUpdateWidget(ChatText old) {
    super.didUpdateWidget(old);
    if (old.text != widget.text) _nodes = parseChatMarkdown(widget.text);
  }

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
      names: widget.names,
      open: (url) {
        final recognizer = TapGestureRecognizer()..onTap = () => widget.onOpen(url);
        _recognizers.add(recognizer);
        return recognizer;
      },
    );
    return renderer.column(_nodes, body);
  }
}

/// Lays Markdown nodes out as widgets and their inline content as spans.
class _Renderer {
  _Renderer({
    required this.colors,
    required this.body,
    required this.mono,
    required this.code,
    required this.link,
    required this.mention,
    required this.names,
    required this.open,
  });

  final MailColors colors;
  final TextStyle body, mono, code, link, mention;
  final Map<String, String> names;
  final GestureRecognizer Function(String url) open;

  /// Between paragraphs and other blocks: half a line.
  static const _blockGap = 8.0;

  static bool _isBlock(md.Node node) => node is md.Element && _blockTags.contains(node.tag);

  /// `nodes` one below the other, [_blockGap] apart (`gap` inside a tight list item): runs of
  /// inline nodes (a tight list item's text) become one paragraph in `style`. Blocks with no text
  /// (a paragraph of spaces) take no room.
  Widget column(List<md.Node> nodes, TextStyle style, {double gap = _blockGap}) {
    final children = <Widget>[];
    final run = <md.Node>[];
    void flush() {
      if (run.isEmpty) return;
      final spans = _trimmed(run);
      if (spans.isNotEmpty) children.add(Text.rich(TextSpan(children: spans), style: style));
      run.clear();
    }

    for (final node in nodes) {
      if (_isBlock(node)) {
        flush();
        final block = _block(node as md.Element, style);
        if (block != null) children.add(block);
      } else {
        run.add(node);
      }
    }
    flush();
    if (children.isEmpty) return const SizedBox.shrink();
    if (children.length == 1) return children.single;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        for (final (i, child) in children.indexed) ...[if (i > 0) SizedBox(height: gap), child],
      ],
    );
  }

  Widget? _block(md.Element element, TextStyle style) {
    final children = element.children ?? const <md.Node>[];
    switch (element.tag) {
      case 'p':
        final spans = _trimmed(children);
        return spans.isEmpty ? null : Text.rich(TextSpan(children: spans), style: style);
      case 'h1' || 'h2' || 'h3' || 'h4' || 'h5' || 'h6':
        final scale = switch (element.tag) { 'h1' => 1.3, 'h2' => 1.18, 'h3' => 1.08, _ => 1.0 };
        final heading = style.copyWith(fontWeight: FontWeight.w700, fontSize: (style.fontSize ?? 14) * scale, height: 1.3);
        return Text.rich(TextSpan(children: _trimmed(children)), style: heading);
      case 'pre':
        var text = element.textContent;
        if (text.endsWith('\n')) text = text.substring(0, text.length - 1);
        return Container(
          width: double.infinity,
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          decoration: BoxDecoration(color: colors.fill, borderRadius: BorderRadius.circular(8)),
          child: Text(text, style: mono.copyWith(height: 1.45)),
        );
      case 'blockquote':
        return Container(
          padding: const EdgeInsets.only(left: 12),
          decoration: BoxDecoration(border: Border(left: BorderSide(color: colors.border, width: 3))),
          child: column(children, style.copyWith(color: colors.secondaryLabel)),
        );
      case 'ul' || 'ol':
        final ordered = element.tag == 'ol';
        var number = int.tryParse(element.attributes['start'] ?? '') ?? 1;
        final items = children.whereType<md.Element>().where((e) => e.tag == 'li').toList();
        // A loose list (items apart by blank lines) spaces its items like paragraphs.
        final loose = items.any((item) => (item.children ?? const []).any((c) => c is md.Element && c.tag == 'p'));
        final marker = style.copyWith(color: colors.secondaryLabel, fontFeatures: const [FontFeature.tabularFigures()]);
        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (final (i, item) in items.indexed)
              Padding(
                padding: EdgeInsets.only(top: i == 0 ? 0 : (loose ? 6 : 2)),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(
                      width: ordered ? 24 : 18,
                      child: Text(ordered ? '${number++}.' : '•', style: marker),
                    ),
                    Expanded(child: column(item.children ?? const [], style, gap: loose ? 6 : 2)),
                  ],
                ),
              ),
          ],
        );
      case 'hr':
        return Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Container(height: 1, color: colors.separator),
        );
      case 'table':
        final rows = <md.Element>[
          for (final section in children.whereType<md.Element>())
            ...(section.children ?? const <md.Node>[]).whereType<md.Element>().where((e) => e.tag == 'tr'),
        ];
        return ClipRRect(
          borderRadius: BorderRadius.circular(8),
          child: Table(
            defaultColumnWidth: const IntrinsicColumnWidth(),
            border: TableBorder.symmetric(inside: BorderSide(color: colors.separator)),
            children: [
              for (final (r, row) in rows.indexed)
                TableRow(
                  decoration: BoxDecoration(color: r == 0 ? colors.fill : null),
                  children: [
                    for (final cell in (row.children ?? const <md.Node>[]).whereType<md.Element>())
                      Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
                        child: Text.rich(
                          TextSpan(children: _trimmed(cell.children ?? const [])),
                          style: cell.tag == 'th' ? style.copyWith(fontWeight: FontWeight.w600) : style,
                        ),
                      ),
                  ],
                ),
            ],
          ),
        );
      default: // li outside a list
        return column(children, style);
    }
  }

  /// Inline nodes as spans, without the spaces and line breaks that end a paragraph.
  List<InlineSpan> _trimmed(List<md.Node> nodes) {
    final spans = [for (final node in nodes) _inline(node)];
    while (spans.isNotEmpty && spans.last is TextSpan && (spans.last as TextSpan).toPlainText().trim().isEmpty) {
      spans.removeLast();
    }
    return spans;
  }

  /// One inline node; `tap` is the link it sits in.
  InlineSpan _inline(md.Node node, [GestureRecognizer? tap]) {
    if (node is md.Text) return TextSpan(text: node.text.replaceAll(_softBreak, ' '), recognizer: tap);
    if (node is! md.Element) return TextSpan(text: node.textContent, recognizer: tap);
    List<InlineSpan> children([GestureRecognizer? inner]) => [
      for (final child in node.children ?? const <md.Node>[]) _inline(child, inner ?? tap),
    ];

    switch (node.tag) {
      case 'strong':
        return TextSpan(style: const TextStyle(fontWeight: FontWeight.w600), children: children());
      case 'em':
        return TextSpan(style: const TextStyle(fontStyle: FontStyle.italic), children: children());
      case 'del':
        return TextSpan(style: const TextStyle(decoration: TextDecoration.lineThrough), children: children());
      case 'code':
        // Thin spaces in the code's fill pad it at both ends, like a chip.
        return TextSpan(text: '\u2009${node.textContent.replaceAll(_softBreak, ' ')}\u2009', style: code, recognizer: tap);
      case 'br':
        return const TextSpan(text: '\n');
      case 'a':
        final href = node.attributes['href'];
        return TextSpan(style: link, children: children(href == null ? null : open(href)));
      case 'img':
        final src = node.attributes['src'];
        final alt = node.attributes['alt'];
        final label = alt == null || alt.isEmpty ? (src ?? '') : alt;
        return TextSpan(text: label, style: link, recognizer: src == null ? tap : open(src));
      case 'mention':
        final user = node.attributes['user'];
        final name = user == null ? node.attributes['email'] : (user == 'users/all' ? 'all' : names[user]);
        return TextSpan(text: '@${name ?? 'user'}', style: mention);
      default:
        return TextSpan(children: children());
    }
  }
}
