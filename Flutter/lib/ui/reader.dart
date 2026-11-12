import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart';
import '../platform.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'compose.dart';
import 'controls.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';
import 'mail_html_windows.dart';
import 'blitz_body.dart';
import 'invitation.dart';
import 'reader_renderer.dart';

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

  /// Conversations between people with more than one message read as a chat log.
  static bool chat(Conversation c) => c.messageCount > 1 && (c.category == null || c.category == MailCategory.people);

  /// The latest and unread messages open; the rest are one-line previews.
  static bool opensExpanded(List<MessageDetail> messages, int index) =>
      index == messages.length - 1 || messages[index].unread;

  /// Loads [c]'s messages ahead of opening it (a pointer resting on its row, a keyboard step),
  /// so opening it draws the bodies at once.
  static void prefetch(MailState state, Conversation c) => state.prefetch(c);

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

  bool get _chat => Reader.chat(c);

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
                expanded: _expanded.contains(m.id) != Reader.opensExpanded(messages, i),
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
        Text(subject.title, style: context.roles.display),
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
            if (account != null) MarkChip(label: account.title, mark: accountColor(state.accountIndex(account.id))),
            for (final role in c.mailboxes.where((r) => r != MailboxRole.inbox && r != MailboxRole.other)) MarkChip(label: role.title),
            if (c.messageCount > 1) MarkChip(label: '${c.messageCount} messages'),
            const ReaderRendererSwitch(),
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
              if (m.invitation case final invitation?)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
                  child: InvitationCard(state: state, conversation: conversation.id, message: m.id, invitation: invitation),
                ),
              Padding(padding: const EdgeInsets.fromLTRB(16, 0, 16, 16), child: _Body(conversation: conversation, message: m, onOpenLink: state.openExternal)),
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
  Widget build(BuildContext context) =>
      MailButton(label: '${attachment.filename}  ·  ${fileSize(attachment.size)}', icon: CupertinoIcons.paperclip, onPressed: onOpen);
}

/// A body: HTML mail flush with the message, as in Mail.app (`.mail-frame` in the native reader);
/// in dark mode it sits on a white card, since mail is designed for a light page. Plain text in
/// the theme's type.
class _Body extends StatelessWidget {
  const _Body({required this.conversation, required this.message, required this.onOpenLink});

  final Conversation conversation;
  final MessageDetail message;
  final ValueChanged<String> onOpenLink;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    if (message.error != null) return Text(message.error!, style: context.text.body.copyWith(color: colors.red));
    final html = message.html;
    if (html != null && html.trim().isNotEmpty) {
      final body = MailHtml(
        key: ValueKey(message.id),
        owner: (account: conversation.account, conversation: conversation.id, message: message.id),
        html: html,
        onOpenLink: onOpenLink,
      );
      // One tree in both modes, so switching the theme keeps the body's views.
      final card = colors.brightness == Brightness.dark;
      return DecoratedBox(
        decoration: BoxDecoration(color: card ? MacosColors.white : null, borderRadius: BorderRadius.circular(Panes.corner)),
        child: Padding(padding: card ? const EdgeInsets.symmetric(horizontal: 16, vertical: 14) : EdgeInsets.zero, child: body),
      );
    }
    return FoldedText(text: message.text ?? '');
  }
}

/// An HTML body at the height of its content. On the Mac the reader's switch picks the renderer
/// ([ReaderRendererScope]): Blitz ([BlitzBody], laid out and painted in Rust) by default, or WebKit
/// (`MailHtmlView` in the runner); Linux draws with Blitz and Windows with WebView2
/// ([WindowsMailHtml]). Every renderer takes the mail as the engine gave it and folds its quoted
/// history behind a toggle by the engine's rules (`FFI/src/mail_html.rs`): Blitz draws the part
/// before the quotes and, once the toggle opens, a page of the quotes; the web views hide the
/// quotes in their page. Only the chosen renderer is mounted; switching keeps the fold.
class MailHtml extends StatefulWidget {
  const MailHtml({super.key, required this.owner, required this.html, required this.onOpenLink});

  /// The message the HTML belongs to: a WebKit page closed with its reader is shown again only
  /// for the same message and HTML.
  final HtmlOwner owner;
  final String html;

  /// Opens a clicked link on Windows; the Mac's and Linux's runners open links themselves.
  final ValueChanged<String> onOpenLink;

  @override
  State<MailHtml> createState() => _MailHtmlState();
}

typedef HtmlOwner = ({String account, String conversation, String message});

class _MailHtmlState extends State<MailHtml> {
  /// Whether the quoted history is open, whichever renderer draws it.
  bool _open = false;

  /// The HTML whose body found quoted history to fold.
  String? _quoted;

  void _toggle() => setState(() => _open = !_open);

  void _found(String html, bool quoted) {
    if (!mounted) return;
    final found = quoted ? html : null;
    if (found != _quoted && (quoted || _quoted == html)) setState(() => _quoted = found);
  }

  @override
  Widget build(BuildContext context) {
    final html = widget.html;
    final quoted = _quoted == html;
    final Widget body;
    if (Host.mac && ReaderRendererScope.current(context) == ReaderRenderer.webKit) {
      final owner = widget.owner;
      body = _WebBody(
        key: ValueKey(html),
        owner: [owner.account, owner.conversation, owner.message],
        html: html,
        open: _open,
        onQuoted: (found) => _found(html, found),
      );
    } else if (!Host.mac && !Host.linux) {
      body = WindowsMailHtml(
        key: ValueKey(html),
        html: html,
        open: _open,
        onOpenLink: widget.onOpenLink,
        onQuoted: (found) => _found(html, found),
      );
    } else {
      // Blitz: the quotes are a page of their own, drawn once the toggle opens them.
      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          BlitzBody(key: ValueKey(html), html: html, part: engine.HtmlPart.main, onQuoted: (found) => _found(html, found)),
          if (quoted)
            _Fold(
              open: _open,
              onToggle: _toggle,
              child: BlitzBody(key: ValueKey(('quoted', html)), html: html, part: engine.HtmlPart.quoted),
            ),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        body,
        if (quoted) Align(alignment: Alignment.topLeft, child: _FoldToggle(open: _open, onTap: _toggle)),
      ],
    );
  }
}

/// One WebKit view of a whole mail, as tall as the page it holds says it is: the engine's page
/// for the mail (`mail_web_document`, under its content policy) with the engine's page script.
/// Its quoted history, if the page finds any ([onQuoted]), shows when [open]. A link clicked in
/// it opens if the engine's rule allows ([BlitzBody.openMailLink]).
class _WebBody extends StatefulWidget {
  const _WebBody({super.key, required this.owner, required this.html, required this.open, required this.onQuoted});

  /// Account, conversation and message.
  final List<String> owner;
  final String html;
  final bool open;

  /// Whether the page has quoted history to fold, each time that changes.
  final ValueChanged<bool> onQuoted;

  @override
  State<_WebBody> createState() => _WebBodyState();
}

class _WebBodyState extends State<_WebBody> {
  /// One channel for every WebKit body, its calls carrying the view id: the engine keeps a channel
  /// it has listened on for good, so a channel per view added one with every body ever shown
  /// (MS-154).
  static const _channel = MethodChannel('mailstream/html');

  /// The bodies showing, by view id, for the heights the runner sends.
  static final _bodies = <int, _WebBodyState>{};

  /// Heights sent before their body listened (its view's creation still on its way back), the
  /// latest per view. A body takes its own when it starts listening and drops those of older
  /// views, whose bodies have taken theirs or gone (view ids only grow).
  static final _early = <int, List<Object?>>{};

  /// The engine's page script and the folder its inline parts are served from, the same for
  /// every page.
  static final _script = engine.mailWebScript(view: engine.MailWebView.webKit);
  static final _inlineCache = engine.inlinePartsFolder();

  static Future<void> _receive(MethodCall call) async {
    final arguments = call.arguments;
    if (call.method == 'link' && arguments is List && arguments.length == 2 && arguments[1] is String) {
      BlitzBody.openMailLink(arguments[1] as String);
      return;
    }
    if (call.method != 'height' || arguments is! List || arguments.length < 3) return;
    final [view, ...height] = arguments;
    final body = _bodies[view];
    if (body != null) {
      body._measured(height);
    } else if (view is int) {
      _early[view] = height;
    }
  }

  int? _view;

  /// Until the page measures itself: about two lines.
  double _height = 40;

  /// The number of the height shown: the page numbers its heights, so an older one arriving late
  /// is dropped.
  int _version = 0;

  /// What the page last said about its quoted history.
  bool _quoted = false;

  /// The body's key is its HTML, so the document is fixed for this state's lifetime.
  late final _document = engine.mailWebDocument(html: widget.html, inline: engine.InlineParts.served);

  void _created(int id) {
    if (!mounted) return;
    _view = id;
    _bodies[id] = this;
    _channel.setMethodCallHandler(_receive);
    final early = _early.remove(id);
    _early.removeWhere((view, _) => view < id);
    if (early != null) _measured(early);
    // A page shown before (or measured before this listened) has its height already: ask for it
    // at this width.
    final box = context.findRenderObject();
    final width = box is RenderBox && box.hasSize ? box.size.width : null;
    _channel.invokeMethod<Object?>('height', {'view': id, 'width': width}).then(_measured, onError: (Object error) {
      // The view can go before it answers.
      if (mounted && _view == id) throw error;
    });
  }

  /// The page's height, its number and whether it has quoted history.
  void _measured(Object? height) {
    if (!mounted || height is! List || height.length < 2) return;
    final version = height[1] as int;
    if (version <= _version) return;
    _version = version;
    setState(() => _height = (height[0] as num).toDouble());
    final quoted = height.length > 2 && height[2] == true;
    if (quoted != _quoted) widget.onQuoted(_quoted = quoted);
  }

  @override
  void didUpdateWidget(_WebBody old) {
    super.didUpdateWidget(old);
    // The view can go before it answers.
    if (old.open != widget.open && _view != null) _channel.invokeMethod<void>('fold', {'view': _view, 'open': widget.open}).ignore();
  }

  @override
  void dispose() {
    if (_view case final view?) _bodies.remove(view);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => SizedBox(
    height: _height,
    child: AppKitView(
      viewType: 'mailstream/html',
      creationParams: {
        'owner': widget.owner,
        'html': _document,
        'script': _script,
        'inlineCache': _inlineCache,
        'open': widget.open,
      },
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

/// Folded content behind a small tonal "more" button ([_FoldToggle]). Open while [open] says so
/// when given (its owner keeps the state, as an HTML body does across renderers), otherwise by
/// its own taps.
class _Fold extends StatefulWidget {
  const _Fold({required this.child, this.open, this.onToggle});

  final Widget child;
  final bool? open;
  final VoidCallback? onToggle;

  @override
  State<_Fold> createState() => _FoldState();
}

class _FoldState extends State<_Fold> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final open = widget.open ?? _open;
    return AnimatedSize(
      duration: Motion.enter,
      curve: Motion.curve,
      alignment: Alignment.topLeft,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _FoldToggle(open: open, onTap: widget.onToggle ?? () => setState(() => _open = !_open)),
          if (open)
            Container(
              padding: const EdgeInsets.only(left: 12),
              decoration: BoxDecoration(border: Border(left: BorderSide(color: context.colors.separator, width: 2))),
              child: widget.child,
            ),
        ],
      ),
    );
  }
}

/// The "more" button that opens and closes quoted history.
class _FoldToggle extends StatelessWidget {
  const _FoldToggle({required this.open, required this.onTap});

  final bool open;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: MacosTooltip(
        message: open ? 'Hide quoted text' : 'Show quoted text',
        child: Hoverable(
          onTap: onTap,
          selected: true,
          selectedColor: colors.strongFill,
          child: SizedBox(width: 32, height: 18, child: Icon(CupertinoIcons.ellipsis, size: 14, color: colors.secondaryLabel)),
        ),
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
        ? _Body(conversation: widget.conversation, message: m, onOpenLink: widget.state.openExternal)
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
                      if (m.invitation case final invitation?)
                        Padding(
                          padding: const EdgeInsets.only(bottom: 8),
                          child: InvitationCard(state: widget.state, conversation: widget.conversation.id, message: m.id, invitation: invitation),
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

  /// The two reply buttons on a capsule floating over the conversation.
  @override
  Widget build(BuildContext context) => Center(
    child: GlassCapsule(
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        MailButton(label: 'Reply', kind: MailButtonKind.primary, onPressed: onReply),
        const SizedBox(width: Space.s6),
        MailButton(label: 'Reply All', onPressed: onReplyAll),
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
              MailTextField(
                controller: _text,
                focusNode: _focus,
                autofocus: true,
                minLines: 3,
                maxLines: 12,
                placeholder: 'Write a reply',
              ),
              const SizedBox(height: 12),
              Row(children: [
                Expanded(
                  child: Text('The original is quoted below your text.  ${Host.shortcut('Return')} sends.',
                      style: text.subheadline.copyWith(color: colors.secondaryLabel)),
                ),
                MailButton(
                  label: _sending ? 'Sending' : 'Send',
                  kind: MailButtonKind.primary,
                  size: MailControlSize.large,
                  busy: _sending,
                  onPressed: _send,
                ),
              ]),
            ],
          ),
        ),
      ),
    );
  }
}
