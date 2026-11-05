import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_widget_from_html_core/flutter_widget_from_html_core.dart';
import 'package:html/dom.dart' as dom;

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';
import 'format.dart';

/// One conversation: a 720 pt reading column with a serif subject and one line of metadata, then
/// either message cards (service mail) or a chat log (people), and a reply pill docked at the
/// bottom. It shows at once from the list's summary; bodies fill in when they arrive.
class Reader extends StatefulWidget {
  const Reader({super.key, required this.state, required this.conversation});

  final MailState state;
  final Conversation conversation;

  static const column = 720.0;

  @override
  State<Reader> createState() => ReaderState();
}

class ReaderState extends State<Reader> {
  final _scroll = ScrollController();
  final Set<String> _expanded = {};
  final _composer = GlobalKey<_ComposerState>();
  bool _composing = false;
  bool _replyAll = false;

  Conversation get c => widget.conversation;

  /// Conversations between people with more than one message read as a chat log.
  bool get _chat => c.messageCount > 1 && (c.category == null || c.category == MailCategory.people);

  @override
  void initState() {
    super.initState();
    _composing = widget.state.drafts.containsKey(c.id);
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  void reply({bool all = false}) {
    setState(() {
      _composing = true;
      _replyAll = all;
    });
    WidgetsBinding.instance.addPostFrameCallback((_) => _composer.currentState?.focus());
  }

  void _toggle(String id) => setState(() => _expanded.contains(id) ? _expanded.remove(id) : _expanded.add(id));

  /// The message the thread opens on: the first unread, else the latest.
  final _focusKey = GlobalKey(debugLabel: 'Thread focus');
  bool _focused = false;

  void _revealFocus(int index) {
    if (_focused) return;
    _focused = true;
    if (index == 0) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final target = _focusKey.currentContext;
      if (target != null && mounted) Scrollable.ensureVisible(target, alignment: 0.02);
    });
  }

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final thread = widget.state.thread(c.id);
    final messages = thread.messages;
    final now = DateTime.now();
    var focus = -1;
    if (messages != null && messages.isNotEmpty) {
      focus = messages.indexWhere((m) => m.unread);
      if (focus < 0) focus = messages.length - 1;
      _revealFocus(focus);
    }

    final body = <Widget>[
      _Header(state: widget.state, conversation: c),
      const SizedBox(height: 22),
      if (messages == null && thread.error == null)
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 40),
          child: Center(child: SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2, color: palette.faint))),
        ),
      if (thread.error != null) Text(thread.error!, style: uiText(TextSize.body, palette.danger)),
      if (messages != null)
        if (_chat)
          _ChatLog(messages: messages, now: now, state: widget.state, onReply: reply, focus: focus, focusKey: _focusKey)
        else
          for (final (i, m) in messages.indexed) ...[
            if (i > 0) Divider(height: 1, thickness: 0.5, color: palette.line),
            _MessageCard(
              key: i == focus ? _focusKey : null,
              message: m,
              state: widget.state,
              now: now,
              // The latest and unread messages open; the rest are one-line previews.
              expanded: _expanded.contains(m.id) != (i == messages.length - 1 || m.unread),
              onToggle: () => _toggle(m.id),
              onReply: reply,
            ),
          ],
      const SizedBox(height: 140),
    ];

    return Container(
      color: palette.page,
      child: Stack(
        children: [
          Scrollbar(
            controller: _scroll,
            child: SingleChildScrollView(
              controller: _scroll,
              padding: const EdgeInsets.fromLTRB(32, 26, 32, 0),
              child: Center(
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: Reader.column),
                  child: SelectionArea(
                    child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: body),
                  ),
                ),
              ),
            ),
          ),
          Positioned(
            left: 32,
            right: 32,
            bottom: 18,
            child: Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: Reader.column),
                child: AnimatedSwitcher(
                  duration: Motion.brief,
                  switchInCurve: houseCurve,
                  switchOutCurve: houseCurve,
                  transitionBuilder: (child, animation) => FadeTransition(
                    opacity: animation,
                    child: SizeTransition(sizeFactor: animation, alignment: Alignment.bottomCenter, child: child),
                  ),
                  child: _composing
                      ? _Composer(
                          key: _composer,
                          state: widget.state,
                          conversation: c,
                          latest: messages?.lastWhere((m) => m.role != MailboxRole.drafts, orElse: () => messages.last),
                          all: _replyAll,
                          onAll: (all) => setState(() => _replyAll = all),
                          onClose: () => setState(() => _composing = false),
                        )
                      : _ReplyDock(onReply: () => reply(), onReplyAll: () => reply(all: true)),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.state, required this.conversation});

  final MailState state;
  final Conversation conversation;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final c = conversation;
    final subject = splitSubject(c.subject);
    final account = state.account(c.account);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          subject.title,
          style: TextStyle(
            fontFamily: FontFamily.serif,
            fontFamilyFallback: FontFamily.serifFallback,
            fontSize: TextSize.display,
            height: 1.2,
            fontWeight: FontWeight.w500,
            letterSpacing: -0.4,
            color: palette.text,
          ),
        ),
        const SizedBox(height: 12),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            if (c.waiting) MetaChip(label: 'Waiting on you', tint: palette.waiting),
            if (subject.tag.isNotEmpty) MetaChip(label: subject.tag),
            for (final label in c.labels) MetaChip(label: label, tint: topicColor(label, context.brightness), mark: ChipMark.dot),
            if (account != null)
              MetaChip(label: account.title, tint: accountPalette[state.accountIndex(account.id) % accountPalette.length], mark: ChipMark.bar),
            for (final role in c.mailboxes.where((r) => r != MailboxRole.inbox && r != MailboxRole.other)) MetaChip(label: role.title),
            if (c.messageCount > 1) MetaChip(label: '${c.messageCount} messages'),
          ],
        ),
      ],
    );
  }
}

// MARK: - Cards

class _MessageCard extends StatefulWidget {
  const _MessageCard({
    super.key,
    required this.message,
    required this.state,
    required this.now,
    required this.expanded,
    required this.onToggle,
    required this.onReply,
  });

  final MessageDetail message;
  final MailState state;
  final DateTime now;
  final bool expanded;
  final VoidCallback onToggle;
  final void Function({bool all}) onReply;

  @override
  State<_MessageCard> createState() => _MessageCardState();
}

class _MessageCardState extends State<_MessageCard> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final m = widget.message;
    final sender = m.sender;
    final head = MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onToggle,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 14),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              InitialsAvatar(address: sender, size: widget.expanded ? 34 : 26),
              const SizedBox(width: 11),
              Expanded(
                child: widget.expanded
                    ? Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text.rich(TextSpan(children: [
                            TextSpan(text: sender.displayName, style: uiText(TextSize.headline, palette.text, weight: FontWeight.w600)),
                            if (sender.name != null) TextSpan(text: '  ${sender.email}', style: uiText(TextSize.footnote, palette.faint)),
                          ]), maxLines: 1, overflow: TextOverflow.ellipsis),
                          const SizedBox(height: 2),
                          Text(
                            'to ${[...m.to, ...m.cc].map((a) => a.displayName).join(', ')}',
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: uiText(TextSize.footnote, palette.muted),
                          ),
                        ],
                      )
                    : Text.rich(
                        TextSpan(children: [
                          TextSpan(text: sender.displayName, style: uiText(TextSize.body, palette.text, weight: FontWeight.w600)),
                          TextSpan(text: '   ${_preview(m)}', style: uiText(TextSize.body, palette.muted)),
                        ]),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
              ),
              const SizedBox(width: 12),
              if (widget.expanded && _hover) ...[
                HoverIconButton(icon: Icons.reply_rounded, tooltip: 'Reply', onPressed: () => widget.onReply()),
                HoverIconButton(icon: Icons.reply_all_rounded, tooltip: 'Reply All', onPressed: () => widget.onReply(all: true)),
                const SizedBox(width: 6),
              ],
              Text(messageDate(m.date, widget.now), style: uiText(TextSize.footnote, palette.muted)),
            ],
          ),
        ),
      ),
    );
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: AnimatedSize(
        duration: Motion.standard,
        curve: houseCurve,
        alignment: Alignment.topCenter,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            head,
            if (widget.expanded) ...[
              Padding(
                padding: const EdgeInsets.only(bottom: 18),
                child: _Body(message: widget.message, state: widget.state),
              ),
              if (m.attachments.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(bottom: 18),
                  child: Wrap(spacing: 8, runSpacing: 8, children: [
                    for (final a in m.attachments) _AttachmentTile(attachment: a, onOpen: () => widget.state.openFile(a.path)),
                  ]),
                ),
            ],
          ],
        ),
      ),
    );
  }

  static String _preview(MessageDetail m) {
    final text = (m.text ?? '').replaceAll(RegExp(r'\s+'), ' ').trim();
    return text.length > 200 ? text.substring(0, 200) : text;
  }
}

class _AttachmentTile extends StatelessWidget {
  const _AttachmentTile({required this.attachment, required this.onOpen});

  final AttachmentInfo attachment;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onOpen,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(Corner.card),
            border: Border.all(color: palette.line),
          ),
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            Icon(Icons.insert_drive_file_outlined, size: 18, color: palette.muted),
            const SizedBox(width: 8),
            Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(attachment.filename, style: uiText(TextSize.body, palette.text, weight: FontWeight.w500)),
              Text(fileSize(attachment.size), style: uiText(TextSize.caption, palette.muted)),
            ]),
          ]),
        ),
      ),
    );
  }
}

/// A body: HTML mail on a light card, as mail is designed for; plain text in the page's type.
class _Body extends StatelessWidget {
  const _Body({required this.message, required this.state});

  final MessageDetail message;
  final MailState state;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    if (message.error != null) return Text(message.error!, style: uiText(TextSize.body, palette.danger));
    final html = message.html;
    if (html != null && html.trim().isNotEmpty) {
      return Container(
        padding: const EdgeInsets.all(18),
        decoration: BoxDecoration(
          color: const Color(0xFFFFFFFF),
          borderRadius: BorderRadius.circular(Corner.card),
          border: context.brightness == Brightness.light ? Border.all(color: palette.line, width: 0.5) : null,
        ),
        child: MailHtml(html: html, state: state),
      );
    }
    return FoldedText(text: message.text ?? '');
  }
}

/// HTML through flutter_widget_from_html: Flutter widgets, no web view. Quoted history folds
/// behind •••; links open in the browser.
class MailHtml extends StatelessWidget {
  const MailHtml({super.key, required this.html, required this.state});

  final String html;
  final MailState state;

  static const _style = TextStyle(fontSize: TextSize.reading, height: 1.45, color: Color(0xFF1D1D1F));

  static bool _isQuote(dom.Element e) =>
      e.classes.contains('gmail_quote') ||
      e.classes.contains('gmail_quote_container') ||
      e.classes.contains('moz-cite-prefix') ||
      e.id == 'divRplyFwdMsg' ||
      e.id == 'appendonsend' ||
      (e.localName == 'blockquote' && e.attributes['type'] == 'cite');

  @override
  Widget build(BuildContext context) => HtmlWidget(
    html,
    textStyle: _style,
    renderMode: RenderMode.column,
    customWidgetBuilder: (element) => _isQuote(element)
        ? _Fold(child: HtmlWidget(element.outerHtml, textStyle: _style, onTapUrl: _open))
        : null,
    customStylesBuilder: (element) => element.localName == 'img' ? const {'max-width': '100%', 'height': 'auto'} : null,
    onTapUrl: _open,
  );

  static Future<bool> _open(String url) async {
    await Process.run('open', [url]);
    return true;
  }
}

/// Plain text with its quoted history and signature folded behind •••.
class FoldedText extends StatelessWidget {
  const FoldedText({super.key, required this.text});

  final String text;

  static final _foldStarts = [
    RegExp(r'^On .{4,240}wrote:\s*$'),
    RegExp(r'^在.{2,200}写道[:：]\s*$'),
    RegExp(r'^-{2,}\s*(Original Message|Forwarded message)\s*-{2,}', caseSensitive: false),
    RegExp(r'^_{10,}\s*$'),
    RegExp(r'^>'),
    RegExp(r'^-- ?$'),
    RegExp(r'^Sent from my ', caseSensitive: false),
    RegExp(r'^Get Outlook for ', caseSensitive: false),
  ];

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    // Text derived from HTML layouts keeps their indentation and empty table cells: drop both.
    final lines = <String>[];
    for (final raw in text.replaceAll('\r\n', '\n').split('\n')) {
      final line = raw.trim();
      if (line.isEmpty && (lines.isEmpty || lines.last.isEmpty)) continue;
      lines.add(line);
    }
    var cut = lines.length;
    for (var i = 1; i < lines.length; i++) {
      if (_foldStarts.any((p) => p.hasMatch(lines[i].trimRight()))) {
        cut = i;
        break;
      }
    }
    String join(Iterable<String> part) => part.join('\n').trim();
    final shown = join(lines.take(cut));
    final folded = join(lines.skip(cut));
    final style = uiText(TextSize.reading, palette.text, height: 1.5);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (shown.isNotEmpty) Text(shown, style: style),
        if (folded.isNotEmpty) _Fold(child: Text(folded, style: style.copyWith(color: palette.muted))),
      ],
    );
  }
}

/// Folded content behind a ••• button.
class _Fold extends StatefulWidget {
  const _Fold({required this.child});

  final Widget child;

  @override
  State<_Fold> createState() => _FoldState();
}

class _FoldState extends State<_Fold> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return AnimatedSize(
      duration: Motion.standard,
      curve: houseCurve,
      alignment: Alignment.topLeft,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 6),
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: () => setState(() => _open = !_open),
                child: Container(
                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 1),
                  decoration: BoxDecoration(color: const Color(0x14000000), borderRadius: BorderRadius.circular(6)),
                  child: Text('•••', style: uiText(TextSize.footnote, const Color(0x99000000), weight: FontWeight.w700)),
                ),
              ),
            ),
          ),
          if (_open)
            Container(
              padding: const EdgeInsets.only(left: 12),
              decoration: BoxDecoration(border: Border(left: BorderSide(color: palette.line, width: 2))),
              child: widget.child,
            ),
        ],
      ),
    );
  }
}

// MARK: - Chat log

/// Like Slack without bubbles: a divider per day, each message under its sender's avatar, name and
/// time; a sender's next message within an hour on the same day drops the avatar and name.
class _ChatLog extends StatelessWidget {
  const _ChatLog({
    required this.messages,
    required this.now,
    required this.state,
    required this.onReply,
    required this.focus,
    required this.focusKey,
  });

  final List<MessageDetail> messages;
  final DateTime now;
  final MailState state;
  final void Function({bool all}) onReply;
  final int focus;
  final GlobalKey focusKey;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final children = <Widget>[];
    MessageDetail? previous;
    for (final (index, m) in messages.indexed) {
      final date = m.date;
      final newDay = date != null && (previous?.date == null || !sameDay(previous!.date!, date));
      if (newDay) {
        children.add(Padding(
          padding: const EdgeInsets.symmetric(vertical: 14),
          child: Row(children: [
            Expanded(child: Divider(thickness: 0.5, color: palette.line)),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: Text(dayDivider(date, now), style: uiText(TextSize.caption, palette.muted, weight: FontWeight.w600)),
            ),
            Expanded(child: Divider(thickness: 0.5, color: palette.line)),
          ]),
        ));
      }
      final continued = !newDay &&
          previous != null &&
          previous.sender.email.toLowerCase() == m.sender.email.toLowerCase() &&
          date != null &&
          previous.date != null &&
          date.difference(previous.date!).inMinutes < 60;
      children.add(_ChatMessage(
        key: index == focus ? focusKey : null,
        message: m,
        continued: continued,
        state: state,
        onReply: onReply,
      ));
      previous = m;
    }
    return Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children);
  }
}

class _ChatMessage extends StatefulWidget {
  const _ChatMessage({super.key, required this.message, required this.continued, required this.state, required this.onReply});

  final MessageDetail message;
  final bool continued;
  final MailState state;
  final void Function({bool all}) onReply;

  @override
  State<_ChatMessage> createState() => _ChatMessageState();
}

class _ChatMessageState extends State<_ChatMessage> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final m = widget.message;
    final text = m.text ?? '';
    final body = text.trim().isEmpty && m.html != null
        ? MailHtml(html: m.html!, state: widget.state)
        : FoldedText(text: text);
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: AnimatedContainer(
        duration: Motion.quick,
        margin: EdgeInsets.only(top: widget.continued ? 2 : 12),
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(color: _hover ? palette.hover : null, borderRadius: BorderRadius.circular(Corner.control)),
        child: Stack(
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                  width: 44,
                  child: widget.continued
                      ? Padding(
                          padding: const EdgeInsets.only(top: 3),
                          child: AnimatedOpacity(
                            opacity: _hover ? 1 : 0,
                            duration: Motion.quick,
                            child: Text(messageTime(m.date), style: uiText(TextSize.micro, palette.faint)),
                          ),
                        )
                      : Align(alignment: Alignment.topLeft, child: InitialsAvatar(address: m.sender, size: 34)),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (!widget.continued)
                        Padding(
                          padding: const EdgeInsets.only(bottom: 3),
                          child: Text.rich(TextSpan(children: [
                            TextSpan(text: m.sender.displayName, style: uiText(TextSize.headline, palette.text, weight: FontWeight.w600)),
                            TextSpan(text: '  ${messageTime(m.date)}', style: uiText(TextSize.footnote, palette.faint)),
                          ])),
                        ),
                      body,
                      if (m.attachments.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.only(top: 8),
                          child: Wrap(spacing: 8, runSpacing: 8, children: [
                            for (final a in m.attachments) _AttachmentTile(attachment: a, onOpen: () => widget.state.openFile(a.path)),
                          ]),
                        ),
                    ],
                  ),
                ),
              ],
            ),
            // Reply floats over a message on hover.
            Positioned(
              right: 0,
              top: 0,
              child: IgnorePointer(
                ignoring: !_hover,
                child: AnimatedOpacity(
                  opacity: _hover ? 1 : 0,
                  duration: Motion.quick,
                  child: Container(
                    decoration: BoxDecoration(
                      color: palette.page,
                      borderRadius: BorderRadius.circular(Corner.control),
                      border: Border.all(color: palette.line, width: 0.5),
                      boxShadow: Shadows.raised,
                    ),
                    child: Row(mainAxisSize: MainAxisSize.min, children: [
                      HoverIconButton(icon: Icons.reply_rounded, tooltip: 'Reply', size: 26, iconSize: 15, onPressed: () => widget.onReply()),
                      HoverIconButton(icon: Icons.reply_all_rounded, tooltip: 'Reply All', size: 26, iconSize: 15, onPressed: () => widget.onReply(all: true)),
                    ]),
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

// MARK: - Reply

class _ReplyDock extends StatelessWidget {
  const _ReplyDock({required this.onReply, required this.onReplyAll});

  final VoidCallback onReply;
  final VoidCallback onReplyAll;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return Center(
      child: Container(
        padding: const EdgeInsets.all(4),
        decoration: BoxDecoration(
          color: palette.page,
          borderRadius: BorderRadius.circular(19),
          border: Border.all(color: palette.line, width: 0.5),
          boxShadow: Shadows.floating,
        ),
        child: Row(mainAxisSize: MainAxisSize.min, children: [
          PillButton(label: 'Reply', icon: Icons.reply_rounded, onPressed: onReply),
          const SizedBox(width: 4),
          PillButton(label: 'Reply All', icon: Icons.reply_all_rounded, onPressed: onReplyAll),
        ]),
      ),
    );
  }
}

/// The inline composer the pill unfolds into. The app plans recipients and quotes the original;
/// this shows who it goes to from what is loaded.
class _Composer extends StatefulWidget {
  const _Composer({
    super.key,
    required this.state,
    required this.conversation,
    required this.latest,
    required this.all,
    required this.onAll,
    required this.onClose,
  });

  final MailState state;
  final Conversation conversation;
  final MessageDetail? latest;
  final bool all;
  final ValueChanged<bool> onAll;
  final VoidCallback onClose;

  @override
  State<_Composer> createState() => _ComposerState();
}

class _ComposerState extends State<_Composer> {
  late final _text = TextEditingController(text: widget.state.drafts[widget.conversation.id] ?? '');
  final _focus = FocusNode();
  bool _sending = false;

  void focus() => _focus.requestFocus();

  @override
  void initState() {
    super.initState();
    _text.addListener(() => widget.state.drafts[widget.conversation.id] = _text.text);
  }

  @override
  void dispose() {
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  String get _recipients {
    final latest = widget.latest;
    if (latest == null) return widget.conversation.from?.displayName ?? '';
    final own = widget.state.accounts.map((a) => a.id.toLowerCase()).toSet();
    final fromMe = latest.from.any((a) => own.contains(a.email.toLowerCase()));
    final primary = fromMe ? latest.to : latest.from;
    final people = [
      ...primary,
      if (widget.all) ...[...latest.to, ...latest.cc, ...latest.from],
    ].where((a) => !own.contains(a.email.toLowerCase()));
    final seen = <String>{};
    return people.where((a) => seen.add(a.email.toLowerCase())).map((a) => a.displayName).join(', ');
  }

  Future<void> _send() async {
    final body = _text.text.trim();
    if (body.isEmpty || _sending) return;
    setState(() => _sending = true);
    final sent = await widget.state.reply(widget.conversation, body, all: widget.all);
    if (!mounted) return;
    setState(() => _sending = false);
    if (sent) {
      widget.state.drafts.remove(widget.conversation.id);
      widget.onClose();
    }
  }

  void _discard() {
    widget.state.drafts.remove(widget.conversation.id);
    widget.onClose();
  }

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.enter, meta: true): _send,
        const SingleActivator(LogicalKeyboardKey.escape): () {
          if (_text.text.trim().isEmpty) {
            _discard();
          } else {
            _focus.unfocus();
          }
        },
      },
      child: Container(
        padding: const EdgeInsets.fromLTRB(16, 12, 12, 12),
        decoration: BoxDecoration(
          color: palette.page,
          borderRadius: BorderRadius.circular(Corner.panel),
          border: Border.all(color: palette.line, width: 0.5),
          boxShadow: Shadows.floating,
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(children: [
              Icon(widget.all ? Icons.reply_all_rounded : Icons.reply_rounded, size: 15, color: palette.muted),
              const SizedBox(width: 6),
              Expanded(
                child: Text.rich(
                  TextSpan(children: [
                    TextSpan(text: 'To ', style: uiText(TextSize.footnote, palette.muted)),
                    TextSpan(text: _recipients, style: uiText(TextSize.footnote, palette.text, weight: FontWeight.w500)),
                  ]),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              HoverIconButton(
                icon: widget.all ? Icons.reply_rounded : Icons.reply_all_rounded,
                tooltip: widget.all ? 'Reply' : 'Reply All',
                onPressed: () => widget.onAll(!widget.all),
              ),
              HoverIconButton(icon: Icons.delete_outline_rounded, tooltip: 'Discard', onPressed: _discard),
            ]),
            const SizedBox(height: 4),
            TextField(
              controller: _text,
              focusNode: _focus,
              autofocus: true,
              minLines: 3,
              maxLines: 12,
              cursorColor: palette.accent,
              style: uiText(TextSize.reading, palette.text, height: 1.45),
              decoration: InputDecoration.collapsed(hintText: 'Write a reply…', hintStyle: uiText(TextSize.reading, palette.faint)),
            ),
            const SizedBox(height: 8),
            Row(children: [
              Text('Quotes the original below your text', style: uiText(TextSize.caption, palette.faint)),
              const Spacer(),
              Text('⌘ Return', style: uiText(TextSize.caption, palette.faint)),
              const SizedBox(width: 8),
              PillButton(label: _sending ? 'Sending…' : 'Send', icon: Icons.arrow_upward_rounded, primary: true, onPressed: _sending ? null : _send),
            ]),
          ],
        ),
      ),
    );
  }
}
