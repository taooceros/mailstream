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

/// One conversation: a reading column with the subject and its chips, then message cards
/// (service mail) or a chat log (people), and reply buttons at the bottom that open an inline
/// composer. It shows at once from the list's summary; bodies fill in when they arrive.
class Reader extends StatefulWidget {
  const Reader({super.key, required this.state, required this.conversation});

  final MailState state;
  final Conversation conversation;

  static const column = 760.0;

  @override
  State<Reader> createState() => ReaderState();
}

class ReaderState extends State<Reader> {
  final _scroll = ScrollController();
  final Set<String> _expanded = {};
  final _composer = GlobalKey<_ComposerState>();
  bool _composing = false;
  bool _replyAll = false;

  /// The message the thread opens on: the first unread, else the latest.
  final _focusKey = GlobalKey(debugLabel: 'Thread focus');
  bool _focused = false;

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
    final colors = context.colors;
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
      const SizedBox(height: 24),
      if (messages == null && thread.error == null)
        const Padding(padding: EdgeInsets.symmetric(vertical: 48), child: Center(child: CircularProgressIndicator())),
      if (thread.error != null) Text(thread.error!, style: context.text.bodyMedium?.copyWith(color: colors.error)),
      if (messages != null)
        if (_chat)
          _ChatLog(messages: messages, now: now, state: widget.state, onReply: reply, focus: focus, focusKey: _focusKey)
        else
          for (final (i, m) in messages.indexed)
            Padding(
              padding: const EdgeInsets.only(bottom: 12),
              child: _MessageCard(
                key: i == focus ? _focusKey : null,
                message: m,
                state: widget.state,
                now: now,
                // The latest and unread messages open; the rest are one-line previews.
                expanded: _expanded.contains(m.id) != (i == messages.length - 1 || m.unread),
                onToggle: () => _toggle(m.id),
                onReply: reply,
              ),
            ),
      const SizedBox(height: 120),
    ];

    return Material(
      color: colors.surface,
      child: Stack(
        children: [
          Scrollbar(
            controller: _scroll,
            child: SingleChildScrollView(
              controller: _scroll,
              padding: const EdgeInsets.fromLTRB(32, 24, 32, 0),
              child: Center(
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: Reader.column),
                  child: SelectionArea(child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: body)),
                ),
              ),
            ),
          ),
          Positioned(
            left: 32,
            right: 32,
            bottom: 20,
            child: Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: Reader.column),
                child: AnimatedSwitcher(
                  duration: Motion.enter,
                  switchInCurve: Motion.curve,
                  switchOutCurve: Motion.curve,
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
                      : _ReplyBar(onReply: () => reply(), onReplyAll: () => reply(all: true)),
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
    final c = conversation;
    final subject = splitSubject(c.subject);
    final account = state.account(c.account);
    final colors = context.colors;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(subject.title, style: context.text.headlineSmall?.copyWith(color: colors.onSurface)),
        const SizedBox(height: 12),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            if (c.waiting)
              Chip(
                visualDensity: VisualDensity.compact,
                avatar: Icon(Icons.reply, size: 16, color: colors.onTertiaryContainer),
                backgroundColor: colors.tertiaryContainer,
                labelStyle: context.text.labelMedium?.copyWith(color: colors.onTertiaryContainer),
                label: const Text('Waiting on you'),
              ),
            if (subject.tag.isNotEmpty) MarkChip(label: subject.tag),
            for (final label in c.labels) MarkChip(label: label, mark: topicColor(label, context.brightness)),
            if (account != null)
              MarkChip(label: account.title, mark: accountPalette[state.accountIndex(account.id) % accountPalette.length], bar: true),
            for (final role in c.mailboxes.where((r) => r != MailboxRole.inbox && r != MailboxRole.other)) MarkChip(label: role.title),
            if (c.messageCount > 1) MarkChip(label: '${c.messageCount} messages'),
          ],
        ),
      ],
    );
  }
}

// MARK: - Cards

class _MessageCard extends StatelessWidget {
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
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final m = message;
    final sender = m.sender;
    return Card.filled(
      color: colors.surfaceContainerLow,
      clipBehavior: Clip.antiAlias,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(16)),
      child: AnimatedSize(
        duration: Motion.enter,
        curve: Motion.curve,
        alignment: Alignment.topCenter,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            InkWell(
              onTap: onToggle,
              child: Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 8, 12),
                child: Row(
                  children: [
                    InitialsAvatar(address: sender),
                    const SizedBox(width: 16),
                    Expanded(
                      child: expanded
                          ? Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Text.rich(
                                  TextSpan(children: [
                                    TextSpan(text: sender.displayName, style: text.titleSmall),
                                    if (sender.name != null)
                                      TextSpan(text: '  <${sender.email}>', style: text.bodySmall?.copyWith(color: colors.onSurfaceVariant)),
                                  ]),
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                ),
                                const SizedBox(height: 2),
                                Text(
                                  'to ${[...m.to, ...m.cc].map((a) => a.displayName).join(', ')}',
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                  style: text.bodySmall?.copyWith(color: colors.onSurfaceVariant),
                                ),
                              ],
                            )
                          : Text.rich(
                              TextSpan(children: [
                                TextSpan(text: sender.displayName, style: text.titleSmall),
                                TextSpan(text: '   ${_preview(m)}', style: text.bodyMedium?.copyWith(color: colors.onSurfaceVariant)),
                              ]),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                    ),
                    const SizedBox(width: 12),
                    Text(messageDate(m.date, now), style: text.labelMedium?.copyWith(color: colors.onSurfaceVariant)),
                    if (expanded) ...[
                      IconButton(tooltip: 'Reply', icon: const Icon(Icons.reply), onPressed: () => onReply()),
                      IconButton(tooltip: 'Reply all', icon: const Icon(Icons.reply_all), onPressed: () => onReply(all: true)),
                    ],
                  ],
                ),
              ),
            ),
            if (expanded) ...[
              Padding(padding: const EdgeInsets.fromLTRB(16, 0, 16, 16), child: _Body(message: m)),
              if (m.attachments.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                  child: Wrap(spacing: 8, runSpacing: 8, children: [
                    for (final a in m.attachments) _AttachmentChip(attachment: a, onOpen: () => state.openFile(a.path)),
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

class _AttachmentChip extends StatelessWidget {
  const _AttachmentChip({required this.attachment, required this.onOpen});

  final AttachmentInfo attachment;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) => ActionChip(
    avatar: const Icon(Icons.attach_file, size: 18),
    label: Text('${attachment.filename}  ·  ${fileSize(attachment.size)}'),
    onPressed: onOpen,
  );
}

/// A body: HTML mail on a white sheet, as mail is designed for; plain text in the theme's type.
class _Body extends StatelessWidget {
  const _Body({required this.message});

  final MessageDetail message;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    if (message.error != null) return Text(message.error!, style: context.text.bodyMedium?.copyWith(color: colors.error));
    final html = message.html;
    if (html != null && html.trim().isNotEmpty) {
      return Container(
        padding: const EdgeInsets.all(16),
        decoration: BoxDecoration(
          color: Colors.white,
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: colors.outlineVariant),
        ),
        child: MailHtml(html: html),
      );
    }
    return FoldedText(text: message.text ?? '');
  }
}

/// HTML through flutter_widget_from_html: Flutter widgets, no web view. Quoted history folds
/// behind a toggle; links open in the browser.
class MailHtml extends StatelessWidget {
  const MailHtml({super.key, required this.html});

  final String html;

  static const _style = TextStyle(fontSize: 14, height: 1.45, color: Color(0xFF1F1F1F));

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

/// Plain text with its quoted history and signature folded behind a toggle.
class FoldedText extends StatelessWidget {
  const FoldedText({super.key, required this.text});

  final String text;

  static final _sentenceEnd = RegExp(r'[.!?:;)>]$');
  static final _runsOn = RegExp(r'^[a-z0-9(]');

  static bool _wrapped(String line, String next) =>
      line.length >= 50 && !line.startsWith('>') && !_sentenceEnd.hasMatch(line) && _runsOn.hasMatch(next);

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
    // Text derived from HTML layouts keeps their indentation and empty table cells: drop both.
    final lines = <String>[];
    for (final raw in text.replaceAll('\r\n', '\n').split('\n')) {
      final line = raw.trim();
      if (line.isEmpty && (lines.isEmpty || lines.last.isEmpty)) continue;
      // Plain text mail is often hard-wrapped near 72 columns: a long line that stops mid-sentence
      // and runs on in lower case is one line, so it reflows to the column's width.
      if (lines.isNotEmpty && _wrapped(lines.last, line)) {
        lines.last = '${lines.last} $line';
        continue;
      }
      lines.add(line);
    }
    var cut = lines.length;
    for (var i = 1; i < lines.length; i++) {
      if (_foldStarts.any((p) => p.hasMatch(lines[i]))) {
        cut = i;
        break;
      }
    }
    String join(Iterable<String> part) => part.join('\n').trim();
    final shown = join(lines.take(cut));
    final folded = join(lines.skip(cut));
    final style = context.text.bodyLarge?.copyWith(height: 1.5);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (shown.isNotEmpty) Text(shown, style: style),
        if (folded.isNotEmpty) _Fold(child: Text(folded, style: style?.copyWith(color: context.colors.onSurfaceVariant))),
      ],
    );
  }
}

/// Folded content behind a small tonal "more" button.
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
    final colors = context.colors;
    return AnimatedSize(
      duration: Motion.enter,
      curve: Motion.curve,
      alignment: Alignment.topLeft,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Tooltip(
              message: _open ? 'Hide quoted text' : 'Show quoted text',
              child: Material(
                color: colors.surfaceContainerHighest,
                borderRadius: BorderRadius.circular(8),
                child: InkWell(
                  borderRadius: BorderRadius.circular(8),
                  onTap: () => setState(() => _open = !_open),
                  child: SizedBox(width: 32, height: 18, child: Icon(Icons.more_horiz, size: 16, color: colors.onSurfaceVariant)),
                ),
              ),
            ),
          ),
          if (_open)
            Container(
              padding: const EdgeInsets.only(left: 12),
              decoration: BoxDecoration(border: Border(left: BorderSide(color: colors.outlineVariant, width: 2))),
              child: widget.child,
            ),
        ],
      ),
    );
  }
}

// MARK: - Chat log

/// A divider per day, each message under its sender's avatar, name and time; a sender's next
/// message within an hour on the same day drops the avatar and name.
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
    final children = <Widget>[];
    MessageDetail? previous;
    for (final (index, m) in messages.indexed) {
      final date = m.date;
      final newDay = date != null && (previous?.date == null || !sameDay(previous!.date!, date));
      if (newDay) {
        children.add(Padding(
          padding: const EdgeInsets.symmetric(vertical: 12),
          child: Row(children: [
            const Expanded(child: Divider()),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: Text(dayDivider(date, now), style: context.text.labelMedium?.copyWith(color: context.colors.onSurfaceVariant)),
            ),
            const Expanded(child: Divider()),
          ]),
        ));
      }
      final continued = !newDay &&
          previous != null &&
          previous.sender.email.toLowerCase() == m.sender.email.toLowerCase() &&
          date != null &&
          previous.date != null &&
          date.difference(previous.date!).inMinutes < 60;
      children.add(_ChatMessage(key: index == focus ? focusKey : null, message: m, continued: continued, state: state, onReply: onReply));
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
    final colors = context.colors;
    final text = context.text;
    final m = widget.message;
    final body = (m.text ?? '').trim().isEmpty && m.html != null ? MailHtml(html: m.html!) : FoldedText(text: m.text ?? '');
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: AnimatedContainer(
        duration: Durations.short3,
        margin: EdgeInsets.only(top: widget.continued ? 0 : 8),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        decoration: BoxDecoration(
          color: _hover ? colors.surfaceContainerLow : Colors.transparent,
          borderRadius: BorderRadius.circular(12),
        ),
        child: Stack(
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                  width: 56,
                  child: widget.continued
                      ? Padding(
                          padding: const EdgeInsets.only(top: 4),
                          child: AnimatedOpacity(
                            opacity: _hover ? 1 : 0,
                            duration: Durations.short3,
                            child: Text(messageTime(m.date), style: text.labelSmall?.copyWith(color: colors.outline)),
                          ),
                        )
                      : Align(alignment: Alignment.topLeft, child: InitialsAvatar(address: m.sender)),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (!widget.continued)
                        Padding(
                          padding: const EdgeInsets.only(bottom: 4),
                          child: Text.rich(TextSpan(children: [
                            TextSpan(text: m.sender.displayName, style: text.titleSmall),
                            TextSpan(text: '  ${messageTime(m.date)}', style: text.labelSmall?.copyWith(color: colors.onSurfaceVariant)),
                          ])),
                        ),
                      body,
                      if (m.attachments.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.only(top: 8),
                          child: Wrap(spacing: 8, runSpacing: 8, children: [
                            for (final a in m.attachments) _AttachmentChip(attachment: a, onOpen: () => widget.state.openFile(a.path)),
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
                  duration: Durations.short3,
                  child: Material(
                    color: colors.surfaceContainerHigh,
                    elevation: 1,
                    shape: const StadiumBorder(),
                    child: Row(mainAxisSize: MainAxisSize.min, children: [
                      IconButton(tooltip: 'Reply', icon: const Icon(Icons.reply, size: 20), onPressed: () => widget.onReply()),
                      IconButton(tooltip: 'Reply all', icon: const Icon(Icons.reply_all, size: 20), onPressed: () => widget.onReply(all: true)),
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

class _ReplyBar extends StatelessWidget {
  const _ReplyBar({required this.onReply, required this.onReplyAll});

  final VoidCallback onReply;
  final VoidCallback onReplyAll;

  /// A floating toolbar: raised over the conversation it scrolls above.
  @override
  Widget build(BuildContext context) => Center(
    child: Material(
      color: context.colors.surfaceContainer,
      elevation: 3,
      shadowColor: context.colors.shadow,
      shape: const StadiumBorder(),
      child: Padding(
        padding: const EdgeInsets.all(8),
        child: Row(mainAxisSize: MainAxisSize.min, children: [
          FilledButton.icon(onPressed: onReply, icon: const Icon(Icons.reply), label: const Text('Reply')),
          const SizedBox(width: 8),
          FilledButton.tonalIcon(onPressed: onReplyAll, icon: const Icon(Icons.reply_all), label: const Text('Reply all')),
        ]),
      ),
    ),
  );
}

/// The inline composer the reply buttons open. The app plans recipients and quotes the original;
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
    final colors = context.colors;
    final text = context.text;
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
      child: Card(
        elevation: 3,
        color: colors.surfaceContainerHigh,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(28)),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 12, 12, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(children: [
                Icon(widget.all ? Icons.reply_all : Icons.reply, size: 20, color: colors.onSurfaceVariant),
                const SizedBox(width: 8),
                Expanded(
                  child: Text.rich(
                    TextSpan(children: [
                      TextSpan(text: 'To  ', style: TextStyle(color: colors.onSurfaceVariant)),
                      TextSpan(text: _recipients),
                    ]),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: text.bodyMedium,
                  ),
                ),
                IconButton(
                  tooltip: widget.all ? 'Reply to sender only' : 'Reply all',
                  icon: Icon(widget.all ? Icons.reply : Icons.reply_all),
                  onPressed: () => widget.onAll(!widget.all),
                ),
                IconButton(tooltip: 'Discard', icon: const Icon(Icons.delete_outline), onPressed: _discard),
              ]),
              const SizedBox(height: 8),
              TextField(
                controller: _text,
                focusNode: _focus,
                autofocus: true,
                minLines: 3,
                maxLines: 12,
                style: text.bodyLarge,
                decoration: const InputDecoration(hintText: 'Write a reply', border: OutlineInputBorder()),
              ),
              const SizedBox(height: 12),
              Row(children: [
                Expanded(
                  child: Text('The original is quoted below your text.  ⌘ Return sends.',
                      style: text.bodySmall?.copyWith(color: colors.onSurfaceVariant)),
                ),
                FilledButton.icon(
                  onPressed: _sending ? null : _send,
                  icon: _sending
                      ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                      : const Icon(Icons.send),
                  label: Text(_sending ? 'Sending' : 'Send'),
                ),
              ]),
            ],
          ),
        ),
      ),
    );
  }
}
