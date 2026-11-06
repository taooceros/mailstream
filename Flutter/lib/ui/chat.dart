import 'package:flutter/cupertino.dart' show CupertinoActivityIndicator, CupertinoIcons;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart';
import '../state/chat_state.dart';
import '../state/mail_state.dart';
import 'chat_markup.dart';
import 'common.dart';
import 'conversation_list.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';

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
          ? const Center(child: CupertinoActivityIndicator())
          : Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  chat.accounts.isEmpty
                      ? 'No account has Google Chat. Sign in to a Google account with Chat access in Mailstream.'
                      : 'No chats yet',
                  textAlign: TextAlign.center,
                  style: context.text.bodyLarge?.copyWith(color: colors.onSurfaceVariant),
                ),
              ),
            );
    } else {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _pageIfNearEnd();
      });
      final footer = chat.more ? 1 : 0;
      body = Scrollbar(
        controller: _scroll,
        child: ListView.builder(
          controller: _scroll,
          padding: const EdgeInsets.only(top: 4, bottom: 24),
          itemCount: entries.length + footer,
          itemExtentBuilder: (index, _) => index < entries.length
              ? (entries[index] is _Section ? ListMetrics.sectionHeight : ListMetrics.rowHeight)
              : (index == entries.length && footer == 1 ? 56 : null),
          itemBuilder: (context, index) {
            if (index == entries.length) {
              return const Center(child: CupertinoActivityIndicator());
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
        ),
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
                    Text('Chat', style: context.text.titleMedium?.copyWith(fontWeight: FontWeight.w700)),
                    if (unread > 0) Text('$unread unread', style: context.text.bodySmall?.copyWith(color: colors.onSurfaceVariant)),
                  ],
                ),
              ),
              GlassCapsule(
                child: Row(mainAxisSize: MainAxisSize.min, children: [
                  IconButton(tooltip: 'Refresh (⌘R)', icon: const Icon(CupertinoIcons.arrow_clockwise), onPressed: chat.refresh),
                  IconButton(
                    tooltip: 'New chat',
                    icon: const Icon(CupertinoIcons.square_pencil),
                    onPressed: chat.accounts.isEmpty ? null : () => openNewChat(context, widget.state),
                  ),
                ]),
              ),
              const SizedBox(width: 12),
            ]),
          ),
        ),
        SizedBox(height: 2, child: chat.loading ? const LinearProgressIndicator() : null),
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
    final weight = space.unread ? FontWeight.w700 : FontWeight.w400;
    final multiAccount = widget.state.chat.accounts.length > 1;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset, vertical: 1),
      child: Material(
        color: widget.selected ? colors.secondaryContainer : Colors.transparent,
        borderRadius: BorderRadius.circular(8),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: widget.onTap,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12),
            child: Row(children: [
              space.kind == 'space' && space.avatarKey.isEmpty
                  ? IconAvatar(icon: CupertinoIcons.person_3_fill, background: colors.tertiary, foreground: Colors.white)
                  : ChatAvatar(state: widget.state, avatarKey: space.avatarKey.isEmpty ? space.space : space.avatarKey,
                      name: space.avatarName, url: space.avatarUrl),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  mainAxisAlignment: MainAxisAlignment.center,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(children: [
                      if (multiAccount) ...[
                        Container(
                          width: 4,
                          height: 14,
                          decoration: BoxDecoration(
                            color: accountPalette[widget.state.accountIndex(space.account) % accountPalette.length],
                            borderRadius: BorderRadius.circular(2),
                          ),
                        ),
                        const SizedBox(width: 6),
                      ],
                      Expanded(
                        child: Text(space.title, maxLines: 1, overflow: TextOverflow.ellipsis,
                            style: context.text.bodyLarge?.copyWith(fontWeight: weight)),
                      ),
                      const SizedBox(width: 8),
                      Text(space.date, style: context.text.labelMedium?.copyWith(
                        color: space.unread ? colors.primary : colors.onSurfaceVariant, fontWeight: weight)),
                    ]),
                    Row(children: [
                      if (space.attachment) ...[
                        Icon(CupertinoIcons.paperclip, size: 13, color: colors.onSurfaceVariant),
                        const SizedBox(width: 2),
                      ],
                      Expanded(
                        child: Text(space.preview, maxLines: 1, overflow: TextOverflow.ellipsis,
                            style: context.text.bodyMedium?.copyWith(
                              color: space.unread ? colors.onSurface : colors.onSurfaceVariant, fontWeight: weight)),
                      ),
                      if (space.unread) ...[
                        const SizedBox(width: 8),
                        Container(width: 8, height: 8, decoration: BoxDecoration(color: colors.primary, shape: BoxShape.circle)),
                      ],
                    ]),
                  ],
                ),
              ),
            ]),
          ),
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

  static OutlineInputBorder _capsule(Color color) =>
      OutlineInputBorder(borderRadius: BorderRadius.circular(20), borderSide: BorderSide(color: color, width: 0.5));

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
            ? Text(thread!.failure!, style: context.text.bodyLarge?.copyWith(color: colors.error))
            : thread != null && thread.loaded
            ? Text('No messages yet', style: context.text.bodyLarge?.copyWith(color: colors.onSurfaceVariant))
            : const CupertinoActivityIndicator(),
      );
    } else {
      final items = _log(thread).reversed.toList();
      // Mentions name members and anyone who wrote here.
      final names = {
        for (final user in thread.members) user.id: user.name,
        for (final message in thread.messages) message.sender.id: message.sender.name,
      };
      final extra = thread.more ? 1 : 0;
      log = Scrollbar(
        controller: _scroll,
        child: ListView.builder(
          controller: _scroll,
          reverse: true,
          padding: const EdgeInsets.fromLTRB(20, 12, 20, 12),
          itemCount: items.length + extra,
          itemBuilder: (context, index) {
            if (index == items.length) {
              return const Padding(
                padding: EdgeInsets.all(12),
                child: Center(child: CupertinoActivityIndicator()),
              );
            }
            return switch (items[index]) {
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
            };
          },
        ),
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
                  child: IconButton(
                    tooltip: widget.twoPane ? 'Close (Esc)' : 'Back (Esc)',
                    icon: Icon(widget.twoPane ? CupertinoIcons.xmark : CupertinoIcons.chevron_left),
                    onPressed: widget.onBack,
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.titleSmall)),
                if (chat.accounts.length > 1)
                  Text(widget.space.account, style: context.text.labelMedium?.copyWith(color: colors.onSurfaceVariant)),
                const SizedBox(width: 8),
              ]),
            ),
          ),
        ),
        const Divider(height: 1),
        Expanded(child: log),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
          child: CallbackShortcuts(
            bindings: {
              const SingleActivator(LogicalKeyboardKey.enter): _send,
              const SingleActivator(LogicalKeyboardKey.escape): widget.onBack,
            },
            child: TextField(
              controller: _text,
              focusNode: _focus,
              minLines: 1,
              maxLines: 6,
              keyboardType: TextInputType.multiline,
              textInputAction: TextInputAction.newline,
              // Messages' field: a capsule with a hairline, the send arrow at its end.
              decoration: InputDecoration(
                hintText: title.isEmpty ? 'Message' : 'Message $title',
                fillColor: colors.surface,
                border: _capsule(colors.outline),
                enabledBorder: _capsule(colors.outline),
                focusedBorder: _capsule(colors.primary),
                contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 9),
                suffixIcon: Padding(
                  padding: const EdgeInsets.only(right: 2),
                  child: ListenableBuilder(
                    listenable: _text,
                    builder: (context, _) => IconButton(
                      tooltip: 'Send (Return)',
                      icon: Icon(CupertinoIcons.arrow_up_circle_fill, size: 24, color: _text.text.trim().isEmpty ? null : colors.primary),
                      onPressed: _text.text.trim().isEmpty || _sending ? null : _send,
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
      const Expanded(child: Divider()),
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12),
        child: Text(dayDivider(date, DateTime.now()),
            style: context.text.labelMedium?.copyWith(color: context.colors.onSurfaceVariant)),
      ),
      const Expanded(child: Divider()),
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
        Text('Card', style: context.text.bodyMedium?.copyWith(color: colors.onSurfaceVariant, fontStyle: FontStyle.italic)),
      if (message.attachments.isNotEmpty)
        Padding(
          padding: const EdgeInsets.only(top: 6),
          child: Wrap(spacing: 8, runSpacing: 8, children: [
            for (final attachment in message.attachments) _AttachmentTile(attachment: attachment, onOpen: state.openExternal),
          ]),
        ),
    ];
    return Padding(
      padding: EdgeInsets.only(top: head ? 10 : 2),
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
                    padding: const EdgeInsets.only(bottom: 2),
                    child: Row(children: [
                      Flexible(
                        child: Text(message.mine ? 'You' : sender.name, maxLines: 1, overflow: TextOverflow.ellipsis,
                            style: context.text.labelLarge?.copyWith(fontWeight: FontWeight.w600)),
                      ),
                      const SizedBox(width: 8),
                      Text(messageTime(created), style: context.text.labelSmall?.copyWith(color: colors.onSurfaceVariant)),
                      if (message.edited != null)
                        Text(' · Edited', style: context.text.labelSmall?.copyWith(color: colors.onSurfaceVariant)),
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
        color: colors.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: colors.outlineVariant.withValues(alpha: 0.6)),
      ),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        Icon(kind.icon, size: 22, color: kind.color),
        const SizedBox(width: 10),
        Flexible(
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            Text(attachment.name, maxLines: 1, overflow: TextOverflow.ellipsis,
                style: context.text.bodyMedium?.copyWith(fontWeight: FontWeight.w500)),
            Text(kind.label, style: context.text.labelSmall?.copyWith(color: colors.onSurfaceVariant)),
          ]),
        ),
      ]),
    );
    if (url == null) return Opacity(opacity: 0.5, child: tile);
    return Tooltip(
      message: 'Open in browser',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(onTap: () => onOpen(url), child: tile),
      ),
    );
  }
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
          Opacity(opacity: failed ? 1 : 0.55, child: Text(pending.text, style: context.text.bodyMedium)),
          Text(failed ? 'Not sent: ${pending.failure}' : 'Sending…',
              style: context.text.labelSmall?.copyWith(color: failed ? colors.error : colors.onSurfaceVariant)),
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
  return showDialog<void>(context: context, builder: (_) => _NewChatDialog(state: state, account: account));
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
    if (!_people.contains(key)) setState(() => _people.add(key));
    _error = null;
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
    final query = _field.text.trim().toLowerCase();
    final suggestions = [
      for (final person in _known)
        if (person.email != null &&
            !_people.contains(person.email!.toLowerCase()) &&
            (query.isEmpty || person.name.toLowerCase().contains(query) || person.email!.toLowerCase().contains(query)))
          person,
    ].take(8).toList();
    return AlertDialog(
      title: const Text('New chat'),
      content: SizedBox(
        width: 460,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (chat.accounts.length > 1) ...[
              DropdownButtonFormField<String>(
                initialValue: _account,
                decoration: const InputDecoration(labelText: 'Account'),
                items: [for (final a in chat.accounts) DropdownMenuItem(value: a.id, child: Text(a.id))],
                onChanged: _starting
                    ? null
                    : (value) {
                        if (value == null || value == _account) return;
                        setState(() {
                          _account = value;
                          _known = const [];
                        });
                        _loadPeople();
                      },
              ),
              const SizedBox(height: 16),
            ],
            if (_people.isNotEmpty) ...[
              Wrap(spacing: 6, runSpacing: 6, children: [
                for (final email in _people)
                  InputChip(label: Text(email), onDeleted: _starting ? null : () => setState(() => _people.remove(email))),
              ]),
              const SizedBox(height: 12),
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
                const SingleActivator(LogicalKeyboardKey.enter, meta: true): _start,
              },
              child: TextField(
                controller: _field,
                focusNode: _focus,
                autofocus: true,
                enabled: !_starting,
                decoration: const InputDecoration(
                  labelText: 'People',
                  hintText: 'Name or email address',
                ),
              ),
            ),
            if (suggestions.isNotEmpty)
              ConstrainedBox(
                constraints: const BoxConstraints(maxHeight: 280),
                child: ListView(
                  shrinkWrap: true,
                  children: [
                    for (final person in suggestions)
                      ListTile(
                        dense: true,
                        leading: ChatAvatar(state: widget.state, avatarKey: person.avatarKey, name: person.name, url: person.avatarUrl, size: 32),
                        title: Text(person.name),
                        subtitle: Text(person.email!),
                        onTap: () {
                          _add(person.email!);
                          _field.clear();
                          _focus.requestFocus();
                        },
                      ),
                  ],
                ),
              ),
            if (!_canStart)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(
                  'This account can reopen its existing direct messages only. To start new chats, sign in to it again in '
                  'Mailstream and allow it to create Google Chat conversations.',
                  style: context.text.bodySmall?.copyWith(color: colors.onSurfaceVariant),
                ),
              ),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(_error!, style: context.text.bodyMedium?.copyWith(color: colors.error)),
              ),
          ],
        ),
      ),
      actions: [
        TextButton(onPressed: _starting ? null : () => Navigator.of(context).pop(), child: const Text('Cancel')),
        FilledButton(
          onPressed: _starting || (_people.isEmpty && _field.text.trim().isEmpty) ? null : _start,
          child: _starting
              ? const CupertinoActivityIndicator(radius: 7, color: Colors.white)
              : Text(_people.length > 1 ? 'Start group chat' : 'Start chat'),
        ),
      ],
    );
  }
}
