import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../backend/models.dart';
import '../platform.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'compose.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';
import 'html_parts.dart';

/// One conversation: a reading column with the subject and its chips, then message cards
/// (service mail) or a chat log (people), and reply buttons at the bottom that open an inline
/// composer. It shows at once from the list's summary; bodies fill in when they arrive.
class Reader extends StatefulWidget {
  const Reader({super.key, required this.state, required this.conversation, this.topInset = 0});

  final MailState state;
  final Conversation conversation;

  /// Height of the toolbar floating over the top, which the message scrolls under.
  final double topInset;

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

  /// A thread opens at its end, the latest message above the reply bar.
  late final _pin = EndPin(_scroll);

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
    _pin.release();
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

  /// Forwards the latest message in a new compose tab: its text under a forwarded-message header
  /// (as the native app's), the caret above it.
  void forward() {
    final messages = widget.state.thread(c).messages;
    final latest = messages?.lastWhere((m) => m.role != MailboxRole.drafts, orElse: () => messages.last);
    final lower = c.subject.trimLeft().toLowerCase();
    final subject = lower.startsWith('fwd:') || lower.startsWith('fw:') ? c.subject : 'Fwd: ${c.subject}';
    final header = <String>[
      '---------- Forwarded message ---------',
      if (latest != null && latest.from.isNotEmpty) 'From: ${latest.from.map(formatRecipient).join(', ')}' else if (c.from != null) 'From: ${formatRecipient(c.from!)}',
      if ((latest?.date ?? c.date) case final date?) 'Date: ${messageDate(date, DateTime.now())}',
      'Subject: ${c.subject}',
      if (latest != null && latest.to.isNotEmpty) 'To: ${latest.to.map(formatRecipient).join(', ')}',
      if (latest != null && latest.cc.isNotEmpty) 'Cc: ${latest.cc.map(formatRecipient).join(', ')}',
    ].join('\n');
    final original = latest?.text?.trim() ?? '';
    widget.state.compose(
      kind: ComposeKind.forward,
      from: c.account,
      subject: subject,
      body: '\n\n${original.isEmpty ? header : '$header\n\n$original'}',
      initialBodySelection: 0,
    );
  }

  void _toggle(String id) {
    _pin.release();
    setState(() => _expanded.contains(id) ? _expanded.remove(id) : _expanded.add(id));
  }

  @override
  Widget build(BuildContext context) {
    final thread = widget.state.thread(c);
    return ListenableBuilder(listenable: thread, builder: (context, _) => _body(context, thread));
  }

  Widget _body(BuildContext context, ThreadEntry thread) {
    final colors = context.colors;
    final messages = thread.messages;
    final now = DateTime.now();

    final body = <Widget>[
      _Header(state: widget.state, conversation: c),
      const SizedBox(height: 24),
      if (messages == null && thread.error == null)
        const Padding(padding: EdgeInsets.symmetric(vertical: 48), child: Center(child: Spinner())),
      if (thread.error != null) Text(thread.error!, style: context.text.body.copyWith(color: colors.red)),
      if (messages != null)
        if (_chat)
          _ChatLog(
            conversation: c,
            messages: messages,
            now: now,
            state: widget.state,
            onReply: reply,
            topInset: widget.topInset,
          )
        else
          for (final (i, m) in messages.indexed)
            Padding(
              padding: const EdgeInsets.only(bottom: 12),
              child: _MessageCard(
                conversation: c,
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

    return ColoredBox(
      color: colors.background,
      child: Stack(
        // Body measurements change the content, not the pane's viewport.
        fit: StackFit.expand,
        children: [
          NotificationListener<Notification>(
            onNotification: _pin.follow,
            child: SingleChildScrollView(
              controller: _scroll,
              padding: EdgeInsets.fromLTRB(32, widget.topInset + 16, 32, 0),
              child: Center(
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: Reader.column),
                  child: Selectable(child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: body)),
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

/// Holds a scroll view at its end while its content grows (bodies loading, pictures taking their
/// size), until the reader scrolls it or [release] lets go. [follow] listens to the notifications
/// of the scroll view [controller] drives.
class EndPin {
  EndPin(this.controller);

  final ScrollController controller;
  bool _held = true;

  void release() => _held = false;

  bool follow(Notification notification) {
    if (notification is UserScrollNotification) {
      _held = false;
    } else if (notification is ScrollMetricsNotification && _held) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (_held && controller.hasClients) controller.jumpTo(controller.position.maxScrollExtent);
      });
    }
    return false;
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
        Text(subject.title, style: context.text.title1.copyWith(color: colors.label, fontWeight: FontWeight.w700)),
        const SizedBox(height: 12),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            if (c.waiting)
              Pill(
                label: 'Waiting on you',
                leading: MacosIcon(CupertinoIcons.arrowshape_turn_up_left_fill, size: 12, color: colors.onWaitingFill),
                background: colors.waitingFill,
                foreground: colors.onWaitingFill,
                deleteIcon: CupertinoIcons.checkmark_circle,
                deleteTooltip: 'Resolve (D)',
                onDelete: () => state.setWaiting(c, waiting: false),
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
    required this.conversation,
    required this.message,
    required this.state,
    required this.now,
    required this.expanded,
    required this.onToggle,
    required this.onReply,
  });

  final Conversation conversation;
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
    // Mail's message: the window's surface inside a hairline, rounded.
    return Container(
      clipBehavior: Clip.antiAlias,
      decoration: ShapeDecoration(
        color: colors.background,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(Panes.corner),
          side: BorderSide(color: colors.separator, width: 0.5),
        ),
      ),
      child: AnimatedSize(
        duration: Motion.enter,
        curve: Motion.curve,
        alignment: Alignment.topCenter,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Hoverable(
              onTap: onToggle,
              radius: 0,
              child: Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 8, 12),
                child: Row(
                  children: [
                    PersonAvatar(address: sender, photo: state.avatar(sender.email)),
                    const SizedBox(width: 16),
                    Expanded(
                      child: expanded
                          ? Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Text.rich(
                                  TextSpan(children: [
                                    TextSpan(text: sender.displayName, style: text.headline),
                                    if (sender.name != null)
                                      TextSpan(text: '  <${sender.email}>', style: text.subheadline.copyWith(color: colors.secondaryLabel)),
                                  ]),
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                ),
                                const SizedBox(height: 2),
                                Text(
                                  'to ${[...m.to, ...m.cc].map((a) => a.displayName).join(', ')}',
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                  style: text.subheadline.copyWith(color: colors.secondaryLabel),
                                ),
                              ],
                            )
                          : Text.rich(
                              TextSpan(children: [
                                TextSpan(text: sender.displayName, style: text.headline),
                                TextSpan(text: '   ${_preview(m)}', style: text.body.copyWith(color: colors.secondaryLabel)),
                              ]),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                    ),
                    const SizedBox(width: 12),
                    Text(messageDate(m.date, now), style: text.callout.copyWith(color: colors.secondaryLabel)),
                    if (expanded) ...[
                      const SizedBox(width: 4),
                      ToolButton(tooltip: 'Reply', icon: CupertinoIcons.arrowshape_turn_up_left, onPressed: () => onReply()),
                      ToolButton(tooltip: 'Reply all', icon: CupertinoIcons.arrowshape_turn_up_left_2, onPressed: () => onReply(all: true)),
                    ],
                  ],
                ),
              ),
            ),
            if (expanded) ...[
              Padding(padding: const EdgeInsets.fromLTRB(16, 0, 16, 16), child: _Body(conversation: conversation, message: m)),
              if (m.attachments.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                  child: Wrap(spacing: 8, runSpacing: 8, children: [
                    for (final a in m.attachments) _AttachmentChip(attachment: a, onOpen: () => state.openExternal(a.path)),
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
  Widget build(BuildContext context) => PushButton(
    controlSize: ControlSize.regular,
    secondary: true,
    onPressed: onOpen,
    child: Row(mainAxisSize: MainAxisSize.min, children: [
      const MacosIcon(CupertinoIcons.paperclip, size: 13),
      const SizedBox(width: 6),
      Text('${attachment.filename}  ·  ${fileSize(attachment.size)}'),
    ]),
  );
}

/// A body: HTML mail on a white sheet, as mail is designed for; plain text in the theme's type.
/// HTML is drawn by WebKit on the Mac; Windows has no HTML view yet (MS-106) and shows the
/// message's text under a notice saying so ([HtmlTextFallback]).
class _Body extends StatelessWidget {
  const _Body({required this.conversation, required this.message});

  final Conversation conversation;
  final MessageDetail message;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    if (message.error != null) return Text(message.error!, style: context.text.body.copyWith(color: colors.red));
    final html = message.html;
    if (html != null && html.trim().isNotEmpty) {
      if (!Host.mac) return HtmlTextFallback(key: ValueKey(message.id), text: message.text, html: html);
      return Container(
        padding: const EdgeInsets.all(16),
        decoration: BoxDecoration(
          color: MacosColors.white,
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: colors.separator),
        ),
        child: MailHtml(
          key: ValueKey(message.id),
          owner: (account: conversation.account, conversation: conversation.id, message: message.id),
          html: html,
        ),
      );
    }
    return FoldedText(text: message.text ?? '');
  }
}

/// An HTML body where the app has no HTML view (Windows, until MS-106): a small notice that the
/// HTML is not shown, then the message's text drawn as plain text ([FoldedText]). The text is
/// the engine's (the message's text part, else its HTML read as text); when that is empty, the
/// HTML's text is read here, in a background isolate ([toText]).
class HtmlTextFallback extends StatefulWidget {
  const HtmlTextFallback({super.key, required this.text, required this.html, this.toText = htmlTextInBackground});

  final String? text;
  final String html;
  final Future<String> Function(String html) toText;

  static const notice = "This message is shown as plain text: HTML mail isn't drawn on Windows yet (MS-106).";

  @override
  State<HtmlTextFallback> createState() => _HtmlTextFallbackState();
}

class _HtmlTextFallbackState extends State<HtmlTextFallback> {
  String? _derived;
  int _request = 0;

  String? get _given => (widget.text?.trim().isEmpty ?? true) ? null : widget.text;

  @override
  void initState() {
    super.initState();
    _derive();
  }

  @override
  void didUpdateWidget(HtmlTextFallback old) {
    super.didUpdateWidget(old);
    if (old.html != widget.html || old.text != widget.text) _derive();
  }

  void _derive() {
    final request = ++_request;
    _derived = null;
    if (_given != null) return;
    widget.toText(widget.html).then((text) {
      if (mounted && request == _request) setState(() => _derived = text);
    }, onError: (Object error, StackTrace stack) {
      FlutterError.reportError(FlutterErrorDetails(exception: error, stack: stack, library: 'mail HTML'));
      if (mounted && request == _request) setState(() => _derived = '');
    });
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = _given ?? _derived;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Padding(
            padding: const EdgeInsets.only(top: 1),
            child: MacosIcon(CupertinoIcons.info_circle, size: 13, color: colors.secondaryLabel),
          ),
          const SizedBox(width: 6),
          Expanded(
            child: Text(HtmlTextFallback.notice, style: context.text.subheadline.copyWith(color: colors.secondaryLabel)),
          ),
        ]),
        const SizedBox(height: 10),
        if (text == null)
          const SizedBox(height: 40, child: Center(child: Spinner()))
        else if (text.trim().isEmpty)
          Text('This message has no text.', style: context.text.body.copyWith(color: colors.secondaryLabel))
        else
          FoldedText(text: text),
      ],
    );
  }
}

/// An HTML body, drawn by WebKit (`MailHtmlView` in the runner) at the height of its content.
/// Quoted history (Gmail, Outlook, Thunderbird, Apple Mail markup) is cut out of the body and
/// folds behind a toggle, in a view of its own made when it opens.
class MailHtml extends StatefulWidget {
  const MailHtml({super.key, required this.owner, required this.html, this.partsCache});

  /// The message the HTML belongs to: a WebKit page closed with its reader is shown again only
  /// for the same message and HTML.
  final HtmlOwner owner;
  final String html;

  /// Overrides preparation for deterministic tests; rendering stays on the native view path.
  final HtmlPartsCache? partsCache;

  @override
  State<MailHtml> createState() => _MailHtmlState();
}

typedef HtmlOwner = ({String account, String conversation, String message});

class _MailHtmlState extends State<MailHtml> {
  HtmlParts? _parts;
  Object? _error;
  int _request = 0;

  HtmlPartsCache get _cache => widget.partsCache ?? HtmlPartsCache.shared;

  @override
  void initState() {
    super.initState();
    _prepare();
  }

  @override
  void didUpdateWidget(MailHtml old) {
    super.didUpdateWidget(old);
    if (old.html != widget.html || old.partsCache != widget.partsCache) _prepare();
  }

  void _prepare() {
    final request = ++_request;
    final html = widget.html;
    _error = null;
    final cached = _cache.peek(html);
    if (cached != null) {
      _parts = cached;
      return;
    }
    // A refresh keeps this message's mounted body/fold until its replacement is ready.
    // Different messages have different keys in _Body, so never show the previous mail.
    _cache.load(html).then((parts) {
      if (!mounted || request != _request) return;
      setState(() => _parts = parts);
    }, onError: (Object error, StackTrace stack) {
      if (!mounted || request != _request) return;
      FlutterError.reportError(FlutterErrorDetails(exception: error, stack: stack, library: 'mail HTML'));
      setState(() => _error = error);
    });
  }

  @override
  Widget build(BuildContext context) {
    final parts = _parts;
    if (parts == null) {
      if (_error != null) return Text('Could not prepare HTML: $_error', style: context.text.body);
      return const SizedBox(height: 40, child: Center(child: Spinner()));
    }
    final (main, quoted) = parts;
    final owner = widget.owner;
    List<String> part(String name) => [owner.account, owner.conversation, owner.message, name];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _WebBody(key: ValueKey(main), owner: part('body'), html: main),
        if (quoted != null) _Fold(child: _WebBody(key: ValueKey(quoted), owner: part('quote'), html: quoted)),
      ],
    );
  }
}

/// One WebKit view, as tall as the page it holds says it is.
class _WebBody extends StatefulWidget {
  const _WebBody({super.key, required this.owner, required this.html});

  /// Account, conversation, message and part (body or quote).
  final List<String> owner;
  final String html;

  @override
  State<_WebBody> createState() => _WebBodyState();
}

class _WebBodyState extends State<_WebBody> {
  MethodChannel? _channel;

  /// Until the page measures itself: about two lines.
  double _height = 40;

  /// The number of the height shown: the page numbers its heights, so an older one arriving late
  /// is dropped.
  int _version = 0;

  void _created(int id) {
    if (!mounted) return;
    final channel = _channel = MethodChannel('mailstream/html/$id')
      ..setMethodCallHandler((call) async {
        if (call.method == 'height') _measured(call.arguments);
      });
    // A page shown before (or measured before this listened) has its height already: ask for it
    // at this width.
    final box = context.findRenderObject();
    final width = box is RenderBox && box.hasSize ? box.size.width : null;
    channel.invokeMethod<Object?>('height', width).then(_measured, onError: (Object error) {
      // The view can go before it answers.
      if (mounted && _channel == channel) throw error;
    });
  }

  void _measured(Object? height) {
    if (!mounted || height is! List || height.length != 2) return;
    final version = height[1] as int;
    if (version <= _version) return;
    _version = version;
    setState(() => _height = (height[0] as num).toDouble());
  }

  @override
  void dispose() {
    _channel?.setMethodCallHandler(null);
    _channel = null;
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => SizedBox(
    height: _height,
    child: AppKitView(
      viewType: 'mailstream/html',
      creationParams: {'owner': widget.owner, 'html': widget.html},
      creationParamsCodec: const StandardMessageCodec(),
      onPlatformViewCreated: _created,
    ),
  );
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
    final style = context.text.body.copyWith(height: 1.5);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (shown.isNotEmpty) Text(shown, style: style),
        if (folded.isNotEmpty) _Fold(child: Text(folded, style: style.copyWith(color: context.colors.secondaryLabel))),
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
            child: MacosTooltip(
              message: _open ? 'Hide quoted text' : 'Show quoted text',
              child: Hoverable(
                onTap: () => setState(() => _open = !_open),
                selected: true,
                selectedColor: colors.strongFill,
                child: SizedBox(width: 32, height: 18, child: Icon(CupertinoIcons.ellipsis, size: 14, color: colors.secondaryLabel)),
              ),
            ),
          ),
          if (_open)
            Container(
              padding: const EdgeInsets.only(left: 12),
              decoration: BoxDecoration(border: Border(left: BorderSide(color: colors.separator, width: 2))),
              child: widget.child,
            ),
        ],
      ),
    );
  }
}

// MARK: - Chat log

/// A conversation between people as a chat log: a divider per day, each message under its
/// sender's avatar, name and time; a sender's next message within an hour on the same day drops
/// the avatar and name. One message at a time is hovered: the log owns which, so moving to the
/// next message always clears the last.
class _ChatLog extends StatefulWidget {
  const _ChatLog({
    required this.conversation,
    required this.messages,
    required this.now,
    required this.state,
    required this.onReply,
    required this.topInset,
  });

  final Conversation conversation;
  final List<MessageDetail> messages;
  final DateTime now;
  final MailState state;
  final void Function({bool all}) onReply;

  /// The reader's toolbar height, which the messages' reply buttons slide under too.
  final double topInset;

  @override
  State<_ChatLog> createState() => _ChatLogState();
}

class _ChatLogState extends State<_ChatLog> {
  final _hovered = ValueNotifier<int?>(null);

  @override
  void dispose() {
    _hovered.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final children = <Widget>[];
    MessageDetail? previous;
    for (final (index, m) in widget.messages.indexed) {
      final date = m.date;
      final newDay = date != null && (previous?.date == null || !sameDay(previous!.date!, date));
      if (newDay) {
        children.add(Padding(
          padding: const EdgeInsets.symmetric(vertical: 12),
          child: Row(children: [
            const Expanded(child: Hairline()),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: Text(dayDivider(date, widget.now), style: context.text.callout.copyWith(color: context.colors.secondaryLabel)),
            ),
            const Expanded(child: Hairline()),
          ]),
        ));
      }
      children.add(_ChatMessage(
        conversation: widget.conversation,
        message: m,
        state: widget.state,
        onReply: widget.onReply,
        index: index,
        hovered: _hovered,
        topInset: widget.topInset,
      ));
      previous = m;
    }
    return Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children);
  }
}

/// A message in the chat log, under its sender's avatar, name and time like every other: each mail
/// stands on its own, with its own sign-off. Hovered, it takes a faint wash, and the reply buttons
/// float on its top edge, over the gap above rather than over the text.
class _ChatMessage extends StatefulWidget {
  const _ChatMessage({
    required this.conversation,
    required this.message,
    required this.state,
    required this.onReply,
    required this.index,
    required this.hovered,
    required this.topInset,
  });

  final Conversation conversation;
  final MessageDetail message;
  final MailState state;
  final void Function({bool all}) onReply;

  /// This message's place in the log, and the log's hovered one.
  final int index;
  final ValueNotifier<int?> hovered;
  final double topInset;

  @override
  State<_ChatMessage> createState() => _ChatMessageState();
}

class _ChatMessageState extends State<_ChatMessage> {
  final _actions = OverlayPortalController();
  final _link = LayerLink();

  bool get _hover => widget.hovered.value == widget.index;

  @override
  void initState() {
    super.initState();
    widget.hovered.addListener(_hoverChanged);
  }

  @override
  void didUpdateWidget(_ChatMessage old) {
    super.didUpdateWidget(old);
    if (old.hovered != widget.hovered) {
      old.hovered.removeListener(_hoverChanged);
      widget.hovered.addListener(_hoverChanged);
    }
    _hoverChanged();
  }

  @override
  void dispose() {
    widget.hovered.removeListener(_hoverChanged);
    super.dispose();
  }

  void _hoverChanged() {
    if (_hover == _actions.isShowing) return;
    _hover ? _actions.show() : _actions.hide();
    setState(() {});
  }

  void _enter(PointerEnterEvent _) => widget.hovered.value = widget.index;

  void _exit(PointerExitEvent _) {
    if (_hover) widget.hovered.value = null;
  }

  /// The reply buttons, floated on the message's top edge and clipped to the part of the reader
  /// below its toolbar, so they slide under the toolbar with the message.
  Widget _actionsOverlay(BuildContext context) {
    final buttons = Align(
      alignment: Alignment.topLeft,
      child: CompositedTransformFollower(
        link: _link,
        targetAnchor: Alignment.topRight,
        followerAnchor: Alignment.topRight,
        offset: const Offset(-12, -18),
        // The buttons sit outside the message, so they keep it hovered themselves.
        child: MouseRegion(
          onEnter: _enter,
          onExit: _exit,
          child: RaisedCapsule(
            child: Row(mainAxisSize: MainAxisSize.min, children: [
              ToolButton(tooltip: 'Reply', icon: CupertinoIcons.arrowshape_turn_up_left, onPressed: () => widget.onReply()),
              ToolButton(
                tooltip: 'Reply all',
                icon: CupertinoIcons.arrowshape_turn_up_left_2,
                onPressed: () => widget.onReply(all: true),
              ),
            ]),
          ),
        ),
      ),
    );
    final viewport = Scrollable.maybeOf(context)?.context.findRenderObject();
    if (viewport is! RenderBox || !viewport.hasSize) return buttons;
    final top = viewport.localToGlobal(Offset(0, widget.topInset));
    final visible = top & Size(viewport.size.width, math.max(0, viewport.size.height - widget.topInset));
    return Positioned.fromRect(rect: visible, child: ClipRect(child: buttons));
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final m = widget.message;
    final hover = _hover;
    final body = (m.text ?? '').trim().isEmpty && m.html != null
        ? _Body(conversation: widget.conversation, message: m)
        : FoldedText(text: m.text ?? '');
    return MouseRegion(
      onEnter: _enter,
      onExit: _exit,
      child: CompositedTransformTarget(
        link: _link,
        child: OverlayPortal(
          controller: _actions,
          overlayChildBuilder: _actionsOverlay,
          child: AnimatedContainer(
            duration: Motion.quick,
            margin: const EdgeInsets.only(top: 12),
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
            decoration: BoxDecoration(
              color: hover ? colors.hover : colors.hover.withValues(alpha: 0),
              borderRadius: BorderRadius.circular(8),
            ),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                  width: 56,
                  child: Align(alignment: Alignment.topLeft, child: PersonAvatar(address: m.sender, photo: widget.state.avatar(m.sender.email))),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Padding(
                        padding: const EdgeInsets.only(bottom: 4),
                        child: Text.rich(TextSpan(children: [
                          TextSpan(text: m.sender.displayName, style: text.headline),
                          TextSpan(text: '  ${messageTime(m.date)}', style: text.subheadline.copyWith(color: colors.secondaryLabel)),
                        ])),
                      ),
                      body,
                      if (m.attachments.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.only(top: 8),
                          child: Wrap(spacing: 8, runSpacing: 8, children: [
                            for (final a in m.attachments) _AttachmentChip(attachment: a, onOpen: () => widget.state.openExternal(a.path)),
                          ]),
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
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

  /// Push buttons on a capsule of the system's glass, floating over the conversation.
  @override
  Widget build(BuildContext context) => Center(
    child: GlassCapsule(
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        PushButton(controlSize: ControlSize.large, onPressed: onReply, child: const Text('Reply')),
        const SizedBox(width: 6),
        PushButton(controlSize: ControlSize.large, secondary: true, onPressed: onReplyAll, child: const Text('Reply All')),
      ]),
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
    final names = people.where((a) => seen.add(a.email.toLowerCase())).map((a) => a.displayName).join(', ');
    // Mail to oneself is answered to oneself.
    return names.isEmpty ? primary.map((a) => a.displayName).join(', ') : names;
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

  /// Moves the reply, as typed so far, into a compose tab so other mail can be read beside it.
  /// It still goes out with `mail.reply`: the app plans its recipients and quotes the original.
  void _popOut() {
    if (_sending) return;
    final rawSubject = widget.conversation.subject;
    widget.state.drafts.remove(widget.conversation.id);
    widget.onClose();
    widget.state.compose(
      kind: ComposeKind.reply,
      replyTo: widget.conversation,
      replyAll: widget.all,
      from: widget.conversation.account,
      to: _recipients,
      subject: rawSubject.trimLeft().toLowerCase().startsWith('re:') ? rawSubject : 'Re: $rawSubject',
      body: _text.text,
      focusBodyInitially: true,
    );
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return CallbackShortcuts(
      bindings: {
        Host.command(LogicalKeyboardKey.enter): _send,
        Host.command(LogicalKeyboardKey.keyO, shift: true): _popOut,
        const SingleActivator(LogicalKeyboardKey.escape): () {
          if (_text.text.trim().isEmpty) {
            _discard();
          } else {
            _focus.unfocus();
          }
        },
      },
      child: Container(
        decoration: ShapeDecoration(
          color: colors.background,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(Panes.sheetCorner),
            side: BorderSide(color: colors.separator, width: 0.5),
          ),
          shadows: [BoxShadow(color: MacosColors.black.withValues(alpha: 0.18), blurRadius: 24, offset: const Offset(0, 8))],
        ),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 12, 12, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(children: [
                Icon(widget.all ? CupertinoIcons.arrowshape_turn_up_left_2 : CupertinoIcons.arrowshape_turn_up_left, size: 17, color: colors.secondaryLabel),
                const SizedBox(width: 8),
                Expanded(
                  child: Text.rich(
                    TextSpan(children: [
                      TextSpan(text: 'To  ', style: TextStyle(color: colors.secondaryLabel)),
                      TextSpan(text: _recipients),
                    ]),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: text.body,
                  ),
                ),
                ToolButton(
                  tooltip: widget.all ? 'Reply to sender only' : 'Reply all',
                  icon: widget.all ? CupertinoIcons.arrowshape_turn_up_left : CupertinoIcons.arrowshape_turn_up_left_2,
                  onPressed: () => widget.onAll(!widget.all),
                ),
                ToolButton(
                  tooltip: 'Open in compose column (${Host.shortcut('O', shift: true)})',
                  icon: CupertinoIcons.sidebar_right,
                  onPressed: _popOut,
                ),
                ToolButton(tooltip: 'Discard', icon: CupertinoIcons.trash, onPressed: _discard),
              ]),
              const SizedBox(height: 8),
              MacosTextField(
                controller: _text,
                focusNode: _focus,
                autofocus: true,
                minLines: 3,
                maxLines: 12,
                style: text.body,
                placeholder: 'Write a reply',
              ),
              const SizedBox(height: 12),
              Row(children: [
                Expanded(
                  child: Text('The original is quoted below your text.  ${Host.shortcut('Return')} sends.',
                      style: text.subheadline.copyWith(color: colors.secondaryLabel)),
                ),
                PushButton(
                  controlSize: ControlSize.large,
                  onPressed: _sending ? null : _send,
                  child: _sending
                      ? const Row(mainAxisSize: MainAxisSize.min, children: [Spinner(radius: 6), SizedBox(width: 6), Text('Sending')])
                      : const Text('Send'),
                ),
              ]),
            ],
          ),
        ),
      ),
    );
  }
}
