import 'dart:io';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;
import 'package:video_player/video_player.dart';

import '../backend/models.dart';
import '../platform.dart';
import '../state/chat_state.dart';
import '../state/mail_state.dart';
import 'chat_markup.dart';
import 'common.dart';
import 'conversation_list.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';
import 'people.dart';
import 'reader.dart';

/// A Chat person's picture: the engine's cached file for an address (Google Chat publishes it
/// there), else Google's own link, else their initial.
class ChatAvatar extends StatelessWidget {
  const ChatAvatar({super.key, required this.state, required this.avatarKey, required this.name, this.url, this.size = ListMetrics.avatarSize});

  final MailState state;

  /// Lowercased address, else `users/…`.
  final String avatarKey;
  final String name;
  final String? url;
  final double size;

  @override
  Widget build(BuildContext context) {
    final email = avatarKey.contains('@') ? avatarKey : '';
    if (email.isNotEmpty) state.requestAvatars([(email, false)]);
    return PersonAvatar(
      address: Address(name: name, email: avatarKey),
      photo: email.isEmpty ? null : state.avatar(email),
      photoUrl: url,
      size: size,
    );
  }
}

sealed class _ListEntry {}

final class _Section extends _ListEntry {
  _Section(this.title);

  final String title;
}

final class _Row extends _ListEntry {
  _Row(this.space);

  final engine.ChatSpace space;
}

/// The chat list pane: header with New chat and refresh, then every account's spaces, most
/// recently active first, in date groups. Lazy, fixed-height rows; the next page of older
/// spaces loads near the end.
class ChatListPane extends StatefulWidget {
  const ChatListPane({super.key, required this.state});

  final MailState state;

  @override
  State<ChatListPane> createState() => _ChatListPaneState();
}

class _ChatListPaneState extends State<ChatListPane> {
  final _scroll = ScrollController();

  ChatState get chat => widget.state.chat;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_pageIfNearEnd);
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  void _pageIfNearEnd() {
    if (!_scroll.hasClients) return;
    final position = _scroll.position;
    if (position.maxScrollExtent - position.pixels < ListMetrics.rowHeight * 8) chat.loadMore();
  }

  List<_ListEntry> _entries() {
    final out = <_ListEntry>[];
    String? section;
    for (final space in chat.spaces) {
      if (space.section != section) {
        section = space.section;
        out.add(_Section(space.section));
      }
      out.add(_Row(space));
    }
    return out;
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final entries = _entries();
    final unread = chat.unreadSpaces;
    final Widget body;
    if (entries.isEmpty) {
      body = !chat.listed || chat.loading
          ? const Center(child: Spinner())
          : Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  chat.accounts.isEmpty
                      ? 'No account has Google Chat. Sign in to a Google account with Chat access in Mailstream.'
                      : 'No chats yet',
                  textAlign: TextAlign.center,
                  style: context.text.body.copyWith(color: colors.secondaryLabel),
                ),
              ),
            );
    } else {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _pageIfNearEnd();
      });
      final footer = chat.more ? 1 : 0;
      // MacosApp's scroll behavior puts the Mac's overlay scrollbar on it.
      body = ListView.builder(
        controller: _scroll,
        padding: const EdgeInsets.only(top: 4, bottom: 24),
        itemCount: entries.length + footer,
        itemExtentBuilder: (index, _) => index < entries.length
            ? (entries[index] is _Section ? ListMetrics.sectionHeight : ListMetrics.rowHeight)
            : (index == entries.length && footer == 1 ? 56 : null),
        itemBuilder: (context, index) {
          if (index == entries.length) {
            return const Center(child: Spinner());
          }
          return switch (entries[index]) {
            _Section(:final title) => SectionHeader(title),
            _Row(:final space) => _ChatRow(
              key: ValueKey('${space.account}|${space.space}'),
              state: widget.state,
              space: space,
              selected: chat.opened == spaceKey(space),
              onTap: () => chat.open(spaceKey(space)),
            ),
          };
        },
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: SizedBox(
            height: 56,
            child: Row(children: [
              const SizedBox(width: 18),
              Expanded(
                child: Column(
                  mainAxisAlignment: MainAxisAlignment.center,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(children: [
                      Text('Chat', style: context.text.title3.copyWith(fontWeight: FontWeight.w700)),
                      if (chat.loading) const Padding(padding: EdgeInsets.only(left: 8), child: Spinner(radius: 6)),
                    ]),
                    if (unread > 0) Text('$unread unread', style: context.text.subheadline.copyWith(color: colors.secondaryLabel)),
                  ],
                ),
              ),
              GlassCapsule(
                child: Row(mainAxisSize: MainAxisSize.min, children: [
                  ToolButton(tooltip: 'Refresh (${Host.shortcut('R')})', icon: CupertinoIcons.arrow_clockwise, onPressed: chat.refresh),
                  ToolButton(
                    tooltip: 'New chat',
                    icon: CupertinoIcons.square_pencil,
                    onPressed: chat.accounts.isEmpty ? null : () => openNewChat(context, widget.state),
                  ),
                ]),
              ),
              const SizedBox(width: 12),
            ]),
          ),
        ),
        Expanded(child: ClipRect(child: body)),
      ],
    );
  }
}

class _ChatRow extends StatefulWidget {
  const _ChatRow({super.key, required this.state, required this.space, required this.selected, required this.onTap});

  final MailState state;
  final engine.ChatSpace space;
  final bool selected;
  final VoidCallback onTap;

  @override
  State<_ChatRow> createState() => _ChatRowState();
}

class _ChatRowState extends State<_ChatRow> {
  @override
  Widget build(BuildContext context) {
    final space = widget.space;
    final colors = context.colors;
    final weight = space.unread ? FontWeight.w700 : FontWeight.w600;
    final multiAccount = widget.state.chat.accounts.length > 1;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset),
      child: LiftedRow(
        selected: widget.selected,
        onTap: widget.onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12),
            child: Row(children: [
              space.kind == 'space' && space.avatarKey.isEmpty
                  ? IconAvatar(icon: CupertinoIcons.person_3_fill, background: colors.waiting, foreground: MacosColors.white)
                  : ChatAvatar(state: widget.state, avatarKey: space.avatarKey.isEmpty ? space.space : space.avatarKey,
                      name: space.avatarName, url: space.avatarUrl),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  mainAxisAlignment: MainAxisAlignment.center,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(children: [
                      Expanded(
                        child: Text(space.title, maxLines: 1, overflow: TextOverflow.ellipsis,
                            style: context.text.body.copyWith(fontWeight: weight)),
                      ),
                      const SizedBox(width: 8),
                      // The account, as in the mail list: a dot before the date.
                      if (multiAccount)
                        Container(
                          margin: const EdgeInsets.only(right: 6),
                          width: 6,
                          height: 6,
                          decoration: BoxDecoration(
                            color: accountPalette[widget.state.accountIndex(space.account) % accountPalette.length],
                            shape: BoxShape.circle,
                          ),
                        ),
                      Text(space.date, style: context.text.callout.copyWith(
                        color: space.unread ? colors.accent : colors.secondaryLabel)),
                    ]),
                    const SizedBox(height: 2),
                    Row(children: [
                      if (space.attachment) ...[
                        Icon(CupertinoIcons.paperclip, size: 13, color: colors.secondaryLabel),
                        const SizedBox(width: 2),
                      ],
                      Expanded(
                        child: Text(space.preview, maxLines: 1, overflow: TextOverflow.ellipsis,
                            style: context.text.body.copyWith(
                              color: space.unread ? colors.label : colors.secondaryLabel,
                              fontWeight: space.unread ? FontWeight.w500 : FontWeight.w400)),
                      ),
                      if (space.unread) ...[
                        const SizedBox(width: 8),
                        Container(width: 7, height: 7, decoration: BoxDecoration(color: colors.accent, shape: BoxShape.circle)),
                      ],
                    ]),
                  ],
                ),
              ),
            ]),
          ),
        ),
    );
  }
}

/// One line of the chat log, newest last.
sealed class _LogItem {}

final class _Day extends _LogItem {
  _Day(this.date);

  final DateTime date;
}

final class _Sent extends _LogItem {
  _Sent(this.message, {required this.head});

  final engine.ChatMessage message;

  /// First of a run from one sender: shows their picture and name.
  final bool head;
}

final class _Pending extends _LogItem {
  _Pending(this.pending);

  final engine.ChatPending pending;
}

/// The open space: toolbar, its messages (newest at the bottom, older pages loading at the top)
/// and the composer. Return sends, ⇧Return starts a new line.
class ChatReader extends StatefulWidget {
  const ChatReader({super.key, required this.state, required this.space, required this.onBack, required this.twoPane});

  final MailState state;
  final SpaceKey space;
  final VoidCallback onBack;
  final bool twoPane;

  @override
  State<ChatReader> createState() => _ChatReaderState();
}

class _ChatReaderState extends State<ChatReader> {
  final _scroll = ScrollController();
  final _text = TextEditingController();
  final _focus = FocusNode(debugLabel: 'Chat composer');
  bool _sending = false;

  ChatState get chat => widget.state.chat;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_earlierIfNearTop);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focus.requestFocus();
    });
  }

  @override
  void dispose() {
    _scroll.dispose();
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  /// The log is reversed: its far end is the oldest message.
  void _earlierIfNearTop() {
    if (!_scroll.hasClients) return;
    final position = _scroll.position;
    if (position.maxScrollExtent - position.pixels < 400) chat.loadEarlier(widget.space);
  }

  Future<void> _send() async {
    final text = _text.text.trim();
    if (text.isEmpty || _sending) return;
    setState(() => _sending = true);
    _text.clear();
    final sent = await chat.send(widget.space, text);
    if (!mounted) return;
    setState(() => _sending = false);
    if (!sent && _text.text.isEmpty) _text.text = text;
    _focus.requestFocus();
  }

  static DateTime? _date(String? iso) => iso == null ? null : DateTime.tryParse(iso)?.toLocal();

  /// Oldest first: day dividers, messages (a sender's run shows their head once), then pending.
  List<_LogItem> _log(engine.ChatThread thread) {
    final out = <_LogItem>[];
    DateTime? day;
    String? sender;
    DateTime? last;
    for (final message in thread.messages) {
      final created = _date(message.created);
      if (created != null && (day == null || !sameDay(day, created))) {
        day = created;
        out.add(_Day(created));
        sender = null;
      }
      final head = sender != message.sender.id || (created != null && last != null && created.difference(last).inMinutes >= 5);
      out.add(_Sent(message, head: head));
      sender = message.sender.id;
      last = created;
    }
    for (final pending in thread.pending) {
      out.add(_Pending(pending));
    }
    return out;
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final thread = chat.thread(widget.space);
    final space = chat.space(widget.space);
    final title = space?.title ?? thread?.title ?? '';
    final Widget log;
    if (thread == null || (thread.messages.isEmpty && thread.pending.isEmpty)) {
      log = Center(
        child: thread?.failure != null
            ? Text(thread!.failure!, style: context.text.body.copyWith(color: colors.red))
            : thread != null && thread.loaded
            ? Text('No messages yet', style: context.text.body.copyWith(color: colors.secondaryLabel))
            : const Spinner(),
      );
    } else {
      final items = _log(thread).reversed.toList();
      // Mentions name members and anyone who wrote here.
      final names = {
        for (final user in thread.members) user.id: user.name,
        for (final message in thread.messages) message.sender.id: message.sender.name,
      };
      final extra = thread.more ? 1 : 0;
      log = ListView.builder(
        controller: _scroll,
        reverse: true,
        padding: const EdgeInsets.fromLTRB(20, 12, 20, 12),
        itemCount: items.length + extra,
        itemBuilder: (context, index) {
          if (index == items.length) {
            return const Padding(
              padding: EdgeInsets.all(12),
              child: Center(child: Spinner()),
            );
          }
          // Messages keep the mail reader's reading width, centred in a wide pane.
          return Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: Reader.column),
              child: switch (items[index]) {
                _Day(:final date) => _DayDivider(date: date),
                _Sent(:final message, :final head) => _MessageRow(
                  key: ValueKey(message.id),
                  state: widget.state,
                  message: message,
                  account: widget.space.account,
                  names: names,
                  head: head,
                ),
                _Pending(:final pending) => _PendingRow(key: ValueKey(pending.id), pending: pending),
              },
            ),
          );
        },
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: SizedBox(
            height: 52,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: Row(children: [
                GlassCapsule(
                  child: ToolButton(
                    tooltip: '${widget.twoPane ? 'Close' : 'Back'} (Esc or ${Host.shortcut('W')})',
                    icon: widget.twoPane ? CupertinoIcons.xmark : CupertinoIcons.chevron_left,
                    onPressed: widget.onBack,
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.headline)),
                if (chat.accounts.length > 1)
                  Text(widget.space.account, style: context.text.callout.copyWith(color: colors.secondaryLabel)),
                const SizedBox(width: 8),
              ]),
            ),
          ),
        ),
        const Hairline(),
        Expanded(child: log),
        Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: Reader.column + 40),
            child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
          child: CallbackShortcuts(
            bindings: {
              const SingleActivator(LogicalKeyboardKey.enter): _send,
              const SingleActivator(LogicalKeyboardKey.escape): widget.onBack,
            },
            // Messages' field: a capsule with a hairline, the send arrow at its end.
            child: MacosTextField(
              controller: _text,
              focusNode: _focus,
              minLines: 1,
              maxLines: 6,
              keyboardType: TextInputType.multiline,
              textInputAction: TextInputAction.newline,
              style: context.text.body,
              placeholder: title.isEmpty ? 'Message' : 'Message $title',
              padding: const EdgeInsets.fromLTRB(14, 7, 8, 7),
              decoration: BoxDecoration(
                color: colors.background,
                border: Border.all(color: colors.border, width: 0.5),
                borderRadius: BorderRadius.circular(18),
              ),
              focusedDecoration: BoxDecoration(
                color: colors.background,
                border: Border.all(color: colors.accent.withValues(alpha: 0.6), width: 1),
                borderRadius: BorderRadius.circular(18),
              ),
              suffix: Padding(
                padding: const EdgeInsets.only(right: 4),
                child: ListenableBuilder(
                  listenable: _text,
                  builder: (context, _) {
                    final empty = _text.text.trim().isEmpty;
                    return MacosIconButton(
                      semanticLabel: 'Send (Return)',
                      icon: MacosIcon(CupertinoIcons.arrow_up_circle_fill, size: 24, color: empty ? colors.border : colors.accent),
                      backgroundColor: MacosColors.transparent,
                      hoverColor: MacosColors.transparent,
                      padding: EdgeInsets.zero,
                      boxConstraints: const BoxConstraints.tightFor(width: 26, height: 26),
                      onPressed: empty || _sending ? null : _send,
                    );
                  },
                ),
              ),
            ),
          ),
        ),
          ),
        ),
      ],
    );
  }
}

class _DayDivider extends StatelessWidget {
  const _DayDivider({required this.date});

  final DateTime date;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 12),
    child: Row(children: [
      const Expanded(child: Hairline()),
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12),
        child: Text(dayDivider(date, DateTime.now()),
            style: context.text.callout.copyWith(color: context.colors.secondaryLabel)),
      ),
      const Expanded(child: Hairline()),
    ]),
  );
}

/// A message as Google Chat lays it out: the sender's picture and name at the head of their run,
/// the text beside it.
class _MessageRow extends StatelessWidget {
  const _MessageRow({
    super.key,
    required this.state,
    required this.message,
    required this.account,
    required this.names,
    required this.head,
  });

  final MailState state;
  final engine.ChatMessage message;

  /// The account's address: the user's own messages show its picture, as Chat on the web does.
  final String account;

  /// `users/<id>` → name, for mentions.
  final Map<String, String> names;
  final bool head;

  static const _avatar = 32.0;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final sender = message.sender;
    final created = DateTime.tryParse(message.created ?? '')?.toLocal();
    final body = <Widget>[
      if (message.markup.isNotEmpty) ChatText(text: message.markup, names: names, onOpen: state.openExternal),
      if (message.markup.isEmpty && message.card)
        Text('Card', style: context.text.body.copyWith(color: colors.secondaryLabel, fontStyle: FontStyle.italic)),
      if (message.attachments.isNotEmpty)
        Padding(
          padding: const EdgeInsets.only(top: 6),
          child: Wrap(spacing: 8, runSpacing: 8, children: [
            for (final attachment in message.attachments) _attachment(attachment),
          ]),
        ),
    ];
    return Padding(
      // A sender's run starts a paragraph's gap below the last; their next messages follow closely.
      padding: EdgeInsets.only(top: head ? 16 : 6),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: _avatar,
            child: head
                ? ChatAvatar(
                    state: state,
                    avatarKey: message.mine && sender.email == null ? account : sender.avatarKey,
                    name: sender.name,
                    url: sender.avatarUrl,
                    size: _avatar,
                  )
                : null,
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (head)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 1),
                    child: Row(crossAxisAlignment: CrossAxisAlignment.baseline, textBaseline: TextBaseline.alphabetic, children: [
                      Flexible(
                        child: Text(message.mine ? 'You' : sender.name, maxLines: 1, overflow: TextOverflow.ellipsis,
                            style: chatReading(context).copyWith(fontWeight: FontWeight.w600, height: 1.3)),
                      ),
                      const SizedBox(width: 8),
                      Text(messageTime(created), style: context.text.callout.copyWith(color: colors.secondaryLabel)),
                      if (message.edited != null)
                        Text(' · Edited', style: context.text.callout.copyWith(color: colors.secondaryLabel)),
                    ]),
                  ),
                ...body,
              ],
            ),
          ),
        ],
      ),
    );
  }

  /// Videos play and pictures show in the log, as Chat on the web does; other files are tiles.
  Widget _attachment(engine.ChatAttachment attachment) {
    final type = attachment.contentType ?? '';
    if (attachment.resource != null) {
      if (type.startsWith('video/')) {
        return _ChatVideo(key: ValueKey(attachment.resource), state: state, account: account, attachment: attachment);
      }
      if (type.startsWith('image/')) {
        return _ChatImage(key: ValueKey(attachment.resource), state: state, account: account, attachment: attachment);
      }
    }
    return _AttachmentTile(attachment: attachment, onOpen: state.openExternal);
  }
}

/// An attachment as Chat on the web shows it: a tile with the file's kind, opened in the browser
/// (Chat's download link or the Drive file). One without a link shows dimmed.
class _AttachmentTile extends StatelessWidget {
  const _AttachmentTile({required this.attachment, required this.onOpen});

  final engine.ChatAttachment attachment;
  final void Function(String url) onOpen;

  ({IconData icon, Color color, String label}) _kind() {
    final type = attachment.contentType ?? '';
    final dot = attachment.name.lastIndexOf('.');
    final ext = dot < 0 ? '' : attachment.name.substring(dot + 1).toLowerCase();
    if (type.startsWith('video/')) return (icon: CupertinoIcons.film, color: const Color(0xFFD93025), label: 'Video');
    if (type.startsWith('image/')) return (icon: CupertinoIcons.photo, color: const Color(0xFFD93025), label: 'Image');
    if (type.startsWith('audio/')) return (icon: CupertinoIcons.music_note, color: const Color(0xFFE8710A), label: 'Audio');
    if (type == 'application/pdf' || ext == 'pdf') return (icon: CupertinoIcons.doc_text, color: const Color(0xFFD93025), label: 'PDF');
    if (attachment.url?.contains('drive.google.com') ?? false) {
      return (icon: CupertinoIcons.doc, color: const Color(0xFF1A73E8), label: 'Google Drive');
    }
    return (icon: CupertinoIcons.doc, color: const Color(0xFF5F6368), label: ext.isEmpty ? 'File' : ext.toUpperCase());
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final url = attachment.url;
    final kind = _kind();
    final tile = Container(
      constraints: const BoxConstraints(minWidth: 220, maxWidth: 340),
      padding: const EdgeInsets.fromLTRB(12, 10, 14, 10),
      decoration: BoxDecoration(
        color: colors.fill,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: colors.separator),
      ),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        MacosIcon(kind.icon, size: 22, color: kind.color),
        const SizedBox(width: 10),
        Flexible(
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            Text(attachment.name, maxLines: 1, overflow: TextOverflow.ellipsis,
                style: context.text.body.copyWith(color: colors.label, fontWeight: FontWeight.w500)),
            Text(kind.label, style: context.text.caption1.copyWith(color: colors.secondaryLabel)),
          ]),
        ),
      ]),
    );
    if (url == null) return Opacity(opacity: 0.5, child: tile);
    return MacosTooltip(
      message: 'Open in browser',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(onTap: () => onOpen(url), child: tile),
      ),
    );
  }
}

/// The largest a picture or video takes in the log.
const _mediaBox = BoxConstraints(maxWidth: 360, maxHeight: 300);

/// A picture sent in Chat, downloaded when its row first shows; clicking opens it in Preview.
class _ChatImage extends StatelessWidget {
  const _ChatImage({super.key, required this.state, required this.account, required this.attachment});

  final MailState state;
  final String account;
  final engine.ChatAttachment attachment;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return FutureBuilder<String>(
      future: state.chat.attachmentFile(account, attachment),
      builder: (context, file) {
        if (file.hasError) return _AttachmentTile(attachment: attachment, onOpen: state.openExternal);
        final path = file.data;
        if (path == null) {
          return Container(
            width: 240,
            height: 160,
            decoration: BoxDecoration(color: colors.fill, borderRadius: BorderRadius.circular(12)),
            child: const Center(child: Spinner()),
          );
        }
        return MacosTooltip(
          message: attachment.name,
          child: MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: () => state.openExternal(path),
              child: ConstrainedBox(
                constraints: _mediaBox,
                child: ClipRRect(
                  borderRadius: BorderRadius.circular(12),
                  child: Image.file(
                    File(path),
                    fit: BoxFit.contain,
                    cacheWidth: (_mediaBox.maxWidth * MediaQuery.devicePixelRatioOf(context)).round(),
                    errorBuilder: (_, _, _) => _AttachmentTile(attachment: attachment, onOpen: state.openExternal),
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}

/// A video sent in Chat, played in the log: clicking downloads it (once) and plays; then a click
/// pauses or resumes, the bar seeks, and the corner button opens it in the system's player
/// (QuickTime on the Mac). `video_player` plays it with AVFoundation on the Mac and, through
/// `video_player_win`, with Media Foundation on Windows.
class _ChatVideo extends StatefulWidget {
  const _ChatVideo({super.key, required this.state, required this.account, required this.attachment});

  final MailState state;
  final String account;
  final engine.ChatAttachment attachment;

  @override
  State<_ChatVideo> createState() => _ChatVideoState();
}

class _ChatVideoState extends State<_ChatVideo> {
  VideoPlayerController? _player;
  String? _path;
  bool _loading = false;
  String? _failure;

  Future<void> _start() async {
    setState(() {
      _loading = true;
      _failure = null;
    });
    try {
      final path = await widget.state.chat.attachmentFile(widget.account, widget.attachment);
      final player = VideoPlayerController.file(File(path));
      await player.initialize();
      if (!mounted) {
        await player.dispose();
        return;
      }
      await player.play();
      setState(() {
        _path = path;
        _player = player;
        _loading = false;
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _loading = false;
        _failure = error is engine.MailException ? describe(error) : 'Could not play this video';
      });
    }
  }

  @override
  void dispose() {
    _player?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final player = _player;
    if (player == null) return _poster(context);
    final colors = context.colors;
    return ConstrainedBox(
      constraints: _mediaBox,
      child: AspectRatio(
        aspectRatio: player.value.aspectRatio,
        child: ClipRRect(
          borderRadius: BorderRadius.circular(12),
          child: ValueListenableBuilder<VideoPlayerValue>(
            valueListenable: player,
            builder: (context, value, _) => Stack(fit: StackFit.expand, children: [
              GestureDetector(
                onTap: () => value.isPlaying ? player.pause() : player.play(),
                child: VideoPlayer(player),
              ),
              if (!value.isPlaying)
                IgnorePointer(
                  child: Center(child: _PlayBadge(icon: CupertinoIcons.play_fill)),
                ),
              Positioned(
                left: 0,
                right: 0,
                bottom: 0,
                child: Container(
                  padding: const EdgeInsets.fromLTRB(10, 14, 6, 6),
                  decoration: const BoxDecoration(
                    gradient: LinearGradient(
                      begin: Alignment.topCenter,
                      end: Alignment.bottomCenter,
                      colors: [Color(0x00000000), Color(0x99000000)],
                    ),
                  ),
                  child: Row(children: [
                    Text(
                      '${_time(value.position)} / ${_time(value.duration)}',
                      style: context.text.caption1.copyWith(color: const Color(0xFFFFFFFF)),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: VideoProgressIndicator(
                        player,
                        allowScrubbing: true,
                        padding: const EdgeInsets.symmetric(vertical: 6),
                        colors: VideoProgressColors(
                          playedColor: colors.accent,
                          bufferedColor: const Color(0x66FFFFFF),
                          backgroundColor: const Color(0x33FFFFFF),
                        ),
                      ),
                    ),
                    MacosIconButton(
                      icon: const MacosIcon(CupertinoIcons.arrow_up_right_square, size: 16, color: Color(0xFFFFFFFF)),
                      onPressed: () => widget.state.openExternal(_path!),
                    ),
                  ]),
                ),
              ),
            ]),
          ),
        ),
      ),
    );
  }

  /// Before playing: a dark frame with the play button and the file's name.
  Widget _poster(BuildContext context) {
    return MouseRegion(
      cursor: _loading ? MouseCursor.defer : SystemMouseCursors.click,
      child: GestureDetector(
        onTap: _loading ? null : _start,
        child: Container(
          width: 320,
          height: 180,
          decoration: BoxDecoration(color: const Color(0xFF202124), borderRadius: BorderRadius.circular(12)),
          child: Stack(children: [
            Center(child: _loading ? const Spinner() : const _PlayBadge(icon: CupertinoIcons.play_fill)),
            Positioned(
              left: 12,
              right: 12,
              bottom: 10,
              child: Text(
                _failure ?? widget.attachment.name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: context.text.caption1.copyWith(color: _failure == null ? const Color(0xDDFFFFFF) : const Color(0xFFFF8A80)),
              ),
            ),
          ]),
        ),
      ),
    );
  }

  static String _time(Duration d) {
    final seconds = (d.inSeconds % 60).toString().padLeft(2, '0');
    return d.inHours > 0 ? '${d.inHours}:${(d.inMinutes % 60).toString().padLeft(2, '0')}:$seconds' : '${d.inMinutes}:$seconds';
  }
}

class _PlayBadge extends StatelessWidget {
  const _PlayBadge({required this.icon});

  final IconData icon;

  @override
  Widget build(BuildContext context) => Container(
    width: 48,
    height: 48,
    decoration: const BoxDecoration(color: Color(0x99000000), shape: BoxShape.circle),
    child: Center(child: MacosIcon(icon, size: 22, color: const Color(0xFFFFFFFF))),
  );
}

/// A message being sent, dimmed, or one Google refused, marked.
class _PendingRow extends StatelessWidget {
  const _PendingRow({super.key, required this.pending});

  final engine.ChatPending pending;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final failed = pending.failure != null;
    return Padding(
      padding: const EdgeInsets.only(top: 6, left: 44),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Opacity(opacity: failed ? 1 : 0.55, child: Text(pending.text, style: chatReading(context))),
          Text(failed ? 'Not sent: ${pending.failure}' : 'Sending…',
              style: context.text.subheadline.copyWith(color: failed ? colors.red : colors.secondaryLabel)),
        ],
      ),
    );
  }
}

/// The New chat dialog: pick the account, add people by address (people this account chats with
/// are suggested), and the direct message or group chat with them opens, found or started.
Future<void> openNewChat(BuildContext context, MailState state) {
  final accounts = state.chat.accounts;
  if (accounts.isEmpty) return Future.value();
  final account = state.chat.opened?.account ?? accounts.first.id;
  return showSheet<void>(context, (_) => _NewChatDialog(state: state, account: account));
}

class _NewChatDialog extends StatefulWidget {
  const _NewChatDialog({required this.state, required this.account});

  final MailState state;
  final String account;

  @override
  State<_NewChatDialog> createState() => _NewChatDialogState();
}

class _NewChatDialogState extends State<_NewChatDialog> {
  late String _account = widget.account;
  final _field = TextEditingController();
  final _focus = FocusNode(debugLabel: 'New chat people');
  final List<String> _people = [];
  List<engine.ChatUser> _known = const [];
  bool _canStart = true;
  bool _starting = false;
  String? _error;

  /// Bumped when the people offered change without an edit: chat partners loaded, someone
  /// added or removed.
  int _revision = 0;

  ChatState get chat => widget.state.chat;

  @override
  void initState() {
    super.initState();
    _loadPeople();
    _field.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _field.dispose();
    _focus.dispose();
    super.dispose();
  }

  Future<void> _loadPeople() async {
    final account = _account;
    try {
      final people = await chat.people(account);
      if (!mounted || account != _account) return;
      setState(() {
        _known = people.people;
        _canStart = people.canStart;
        _revision++;
      });
    } on engine.MailException {
      // Suggestions stay empty; typed addresses still work.
    }
  }

  static final _email = RegExp(r'^[^@\s,;<>]+@[^@\s,;<>]+\.[^@\s,;<>]+$');

  /// Takes the typed addresses (split at commas, semicolons and spaces) into the list.
  bool _commitTyped() {
    final parts = _field.text.split(RegExp(r'[,;\s]+')).where((p) => p.isNotEmpty).toList();
    final bad = parts.where((p) => !_email.hasMatch(p)).toList();
    for (final part in parts.where(_email.hasMatch)) {
      _add(part);
    }
    _field.text = bad.join(' ');
    if (bad.isNotEmpty) setState(() => _error = '"${bad.first}" is not an email address.');
    return bad.isEmpty;
  }

  void _add(String email) {
    final key = email.toLowerCase();
    if (!_people.contains(key)) {
      setState(() {
        _people.add(key);
        _revision++;
      });
    }
    _error = null;
  }

  /// People to complete to, as the native New Chat sheet offers them: the account's chat
  /// partners first (all of them while nothing is typed), then everyone `contacts.search` finds;
  /// an address typed out in full first of all.
  Future<List<Suggestion>> _suggest(String text) async {
    final query = text.trim().toLowerCase();
    final partners = [
      for (final person in _known)
        if (person.email case final email?
            when !_people.contains(email.toLowerCase()) &&
                (query.isEmpty || person.name.toLowerCase().contains(query) || email.toLowerCase().contains(query)))
          Suggestion(Address(name: person.name, email: email), photoUrl: person.avatarUrl),
    ];
    final seen = {..._people, for (final p in partners) p.address.email.toLowerCase()};
    final contacts = query.isEmpty ? const <Address>[] : await widget.state.searchContacts(query);
    final people = [
      ...partners,
      for (final person in contacts)
        if (seen.add(person.email.toLowerCase())) Suggestion(person),
    ];
    final exact = people.indexWhere((p) => p.address.email.toLowerCase() == query);
    if (exact > 0) people.insert(0, people.removeAt(exact));
    return people.take(8).toList();
  }

  Future<void> _start() async {
    if (!_commitTyped() || _people.isEmpty || _starting) return;
    setState(() {
      _starting = true;
      _error = null;
    });
    try {
      await chat.start(_account, List.of(_people));
      if (mounted) Navigator.of(context).pop();
    } on engine.MailException catch (error) {
      if (!mounted) return;
      setState(() {
        _starting = false;
        _error = error is engine.FailedMailException ? error.message : '$error';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return Sheet(
      width: 460,
      title: 'New chat',
      actions: [
        PushButton(
          controlSize: ControlSize.large,
          secondary: true,
          onPressed: _starting ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        PushButton(
          controlSize: ControlSize.large,
          onPressed: _starting || (_people.isEmpty && _field.text.trim().isEmpty) ? null : _start,
          child: _starting
              ? const Row(mainAxisSize: MainAxisSize.min, children: [Spinner(radius: 6), SizedBox(width: 6), Text('Starting')])
              : Text(_people.length > 1 ? 'Start group chat' : 'Start chat'),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (chat.accounts.length > 1) ...[
              Row(children: [
                Text('Account', style: text.body.copyWith(color: colors.secondaryLabel)),
                const SizedBox(width: 10),
                MacosPopupButton<String>(
                  value: _account,
                  items: [for (final a in chat.accounts) MacosPopupMenuItem(value: a.id, child: Text(a.id))],
                  onChanged: _starting
                      ? null
                      : (value) {
                          if (value == null || value == _account) return;
                          setState(() {
                            _account = value;
                            _known = const [];
                            _revision++;
                          });
                          _loadPeople();
                        },
                ),
              ]),
              const SizedBox(height: 14),
            ],
            if (_people.isNotEmpty) ...[
              Wrap(spacing: 6, runSpacing: 6, children: [
                for (final email in _people)
                  Pill(
                    label: email,
                    onDelete: _starting
                        ? null
                        : () => setState(() {
                            _people.remove(email);
                            _revision++;
                          }),
                  ),
              ]),
              const SizedBox(height: 10),
            ],
            CallbackShortcuts(
              bindings: {
                const SingleActivator(LogicalKeyboardKey.enter): () {
                  if (_field.text.trim().isEmpty) {
                    _start();
                  } else {
                    _commitTyped();
                  }
                },
                Host.command(LogicalKeyboardKey.enter): _start,
              },
              child: PeopleCompletion(
                state: widget.state,
                controller: _field,
                focusNode: _focus,
                suggest: _suggest,
                revision: _revision,
                onPick: (person) {
                  _add(person.address.email);
                  _field.clear();
                },
                child: MacosTextField(
                  controller: _field,
                  focusNode: _focus,
                  autofocus: true,
                  enabled: !_starting,
                  style: text.body,
                  placeholder: 'Name or email address',
                  padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 5),
                ),
              ),
            ),
            if (!_canStart)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(
                  'This account can reopen its existing direct messages only. To start new chats, sign in to it again in '
                  'Mailstream and allow it to create Google Chat conversations.',
                  style: text.subheadline.copyWith(color: colors.secondaryLabel),
                ),
              ),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(_error!, style: text.body.copyWith(color: colors.red)),
              ),
          ],
        ),
      ),
    );
  }
}
