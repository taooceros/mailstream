import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../backend/control_client.dart';
import '../backend/models.dart';

/// What the list shows: one role across every account, or one account's mailbox.
sealed class Selection {
  const Selection();

  String get key;
  MailboxRole get role;
  Map<String, Object?> get listArgs;
}

final class UnifiedSelection extends Selection {
  const UnifiedSelection(this.role);

  @override
  final MailboxRole role;

  @override
  String get key => 'unified:${role.name}';

  @override
  Map<String, Object?> get listArgs => {'mailbox': [role.name]};

  @override
  bool operator ==(Object other) => other is UnifiedSelection && other.role == role;

  @override
  int get hashCode => role.hashCode;
}

final class MailboxSelection extends Selection {
  const MailboxSelection(this.mailbox);

  final Mailbox mailbox;

  @override
  MailboxRole get role => mailbox.role;

  @override
  String get key => 'mailbox:${mailbox.account}:${mailbox.id}';

  @override
  Map<String, Object?> get listArgs => {'mailbox': [mailbox.id], 'account': mailbox.account};

  @override
  bool operator ==(Object other) => other is MailboxSelection && other.key == key;

  @override
  int get hashCode => key.hashCode;
}

/// One fetched list and how fresh it is.
class ListEntry {
  List<Conversation> items = const [];
  DateTime? fetchedAt;
  bool loading = false;
  String? error;

  /// Answers of fetches older than the latest are dropped.
  int generation = 0;
}

/// A conversation's messages, as `mail show` returned them.
class ThreadEntry {
  ThreadEntry(this.future);

  final Future<List<MessageDetail>> future;
  List<MessageDetail>? messages;
  String? error;
}

/// A short-lived line at the bottom of the window.
class Toast {
  Toast(this.text, {this.error = false});

  final String text;
  final bool error;
}

/// The UI's single source of truth. The Swift app behind the control socket owns sync, caching,
/// threading and tagging; this keeps what the window shows, serves it from memory while fresh, and
/// applies the user's actions optimistically, rolling them back when the app reports a failure.
class MailState extends ChangeNotifier {
  MailState(this.client);

  final ControlClient client;

  /// A list fetched within this long serves reads without asking again; the native app polls at
  /// the same pace while push is down.
  static const listLifetime = Duration(seconds: 90);
  static const pageSize = 100;

  List<Account> accounts = const [];
  Map<String, List<Mailbox>> mailboxes = const {};
  Selection selection = const UnifiedSelection(MailboxRole.inbox);

  /// Set while the Smart Inbox bundle of that category lies over the list.
  MailCategory? openBundle;

  /// The conversation in the reader.
  Conversation? opened;

  String? searchQuery;
  final ListEntry _search = ListEntry();
  Timer? _searchDebounce;

  /// Nil while the app answers; else why it does not.
  String? unreachable;
  Toast? toast;
  Timer? _toastTimer;

  final Map<String, ListEntry> _lists = {};
  final Map<String, ThreadEntry> _threads = {};
  Timer? _poll;

  /// Whether the running app knows `mail.show --html`; a build without it gets text bodies.
  bool _htmlBodies = true;

  /// Inline reply drafts by conversation, kept until sent or discarded (in memory, like the app).
  final Map<String, String> drafts = {};

  // MARK: - Startup

  Future<void> start() async {
    await reloadAccounts();
    await _fetch(selection);
    _poll = Timer.periodic(listLifetime, (_) => refresh(quiet: true));
  }

  @override
  void dispose() {
    _poll?.cancel();
    _searchDebounce?.cancel();
    _toastTimer?.cancel();
    super.dispose();
  }

  Future<void> reloadAccounts() async {
    try {
      final list = await client.call('accounts.list') as List;
      final boxes = await client.call('mailboxes') as Map<String, Object?>;
      accounts = list.map((a) => Account.fromJson(a as Map<String, Object?>)).toList();
      mailboxes = {
        for (final entry in boxes.entries)
          entry.key: (entry.value as List).map((b) => Mailbox.fromJson(entry.key, b as Map<String, Object?>)).toList(),
      };
      unreachable = null;
    } on ControlException catch (error) {
      if (error.code == 'unreachable') unreachable = error.message;
      _report(error.message);
    }
    notifyListeners();
  }

  /// Fetches the list in view and the counts again.
  Future<void> refresh({bool quiet = false}) async {
    if (!quiet) unawaited(client.call('refresh').then((_) {}, onError: (_) {}));
    await Future.wait([reloadAccounts(), _fetch(selection)]);
  }

  // MARK: - Accounts and mailboxes

  int accountIndex(String account) => accounts.indexWhere((a) => a.id == account).clamp(0, 1 << 20);

  Account? account(String id) => accounts.where((a) => a.id == id).firstOrNull;

  Mailbox? mailbox(String account, MailboxRole role) =>
      mailboxes[account]?.where((b) => b.role == role).firstOrNull ??
      (role == MailboxRole.archive ? mailboxes[account]?.where((b) => b.role == MailboxRole.all).firstOrNull : null);

  /// Each account's mailbox for `role`, in sidebar order.
  List<Mailbox> unified(MailboxRole role) => [for (final a in accounts) ?mailbox(a.id, role)];

  int unreadCount(Selection selection) => switch (selection) {
    UnifiedSelection(:final role) => unified(role).fold(0, (sum, b) => sum + (b.unread ?? 0)),
    MailboxSelection(:final mailbox) => mailbox.unread ?? 0,
  };

  // MARK: - Lists

  ListEntry get _current => searchQuery != null ? _search : (_lists[selection.key] ??= ListEntry());

  bool get isLoading => _current.loading;
  String? get listError => _current.error;

  /// The conversations in view, before the Smart Inbox folds bundles.
  List<Conversation> get conversations {
    final query = searchQuery;
    if (query == null) return _current.items;
    if (_search.fetchedAt != null) return _search.items;
    // Until the providers answer, matches among what is already loaded show at once.
    final words = query.toLowerCase().split(RegExp(r'\s+')).where((w) => w.isNotEmpty && !w.contains(':')).toList();
    if (words.isEmpty) return const [];
    final seen = <String>{};
    return [
      for (final entry in _lists.values)
        for (final c in entry.items)
          if (words.every((w) => _haystack(c).contains(w)) && seen.add(c.id)) c,
    ]..sort((a, b) => (b.date ?? DateTime(0)).compareTo(a.date ?? DateTime(0)));
  }

  static String _haystack(Conversation c) =>
      '${c.subject} ${c.participants.join(' ')} ${c.from?.email ?? ''} ${c.snippet ?? ''}'.toLowerCase();

  /// The Smart Inbox folds service mail into bundles in the inbox, outside search.
  bool get bundlesEnabled => searchQuery == null && selection.role == MailboxRole.inbox;

  List<Conversation> bundleItems(MailCategory category) => conversations.where((c) => c.bundle == category).toList();

  void select(Selection next) {
    _searchDebounce?.cancel();
    searchQuery = null;
    openBundle = null;
    opened = null;
    if (next == selection) {
      notifyListeners();
      return;
    }
    selection = next;
    notifyListeners();
    final entry = _lists[next.key];
    final fresh = entry?.fetchedAt != null && DateTime.now().difference(entry!.fetchedAt!) < listLifetime;
    if (!fresh) _fetch(next);
  }

  Future<void> _fetch(Selection target) async {
    final entry = _lists[target.key] ??= ListEntry();
    final generation = ++entry.generation;
    entry.loading = true;
    notifyListeners();
    try {
      final result = await client.call('mail.list', {...target.listArgs, 'limit': pageSize}) as Map<String, Object?>;
      if (generation != entry.generation) return;
      entry.items = (result['conversations'] as List).map((c) => Conversation.fromJson(c as Map<String, Object?>)).toList();
      entry.fetchedAt = DateTime.now();
      final failures = (result['failures'] as List?)?.cast<String>() ?? const [];
      entry.error = failures.isEmpty ? null : failures.join('\n');
      unreachable = null;
    } on ControlException catch (error) {
      if (generation != entry.generation) return;
      entry.error = error.message;
      if (error.code == 'unreachable') unreachable = error.message;
    } finally {
      if (generation == entry.generation) {
        entry.loading = false;
        notifyListeners();
      }
    }
  }

  // MARK: - Search

  void search(String? query) {
    final text = query?.trim();
    _searchDebounce?.cancel();
    if (text == null || text.isEmpty) {
      if (searchQuery == null) return;
      searchQuery = null;
      _search.generation++;
      _search.loading = false;
      notifyListeners();
      return;
    }
    searchQuery = text;
    openBundle = null;
    _search
      ..generation += 1
      ..items = const []
      ..fetchedAt = null
      ..error = null
      ..loading = true;
    notifyListeners();
    final generation = _search.generation;
    // Local matches show at once; the providers' search runs once typing pauses.
    _searchDebounce = Timer(const Duration(milliseconds: 450), () async {
      try {
        final result = await client.call('mail.search', {'query': text, 'limit': 60}) as Map<String, Object?>;
        if (generation != _search.generation) return;
        _search
          ..items = (result['conversations'] as List).map((c) => Conversation.fromJson(c as Map<String, Object?>)).toList()
          ..fetchedAt = DateTime.now();
      } on ControlException catch (error) {
        if (generation != _search.generation) return;
        _search.error = error.message;
      }
      _search.loading = false;
      notifyListeners();
    });
  }

  // MARK: - Bundles and reader

  void showBundle(MailCategory? category) {
    openBundle = category;
    notifyListeners();
  }

  void open(Conversation conversation) {
    opened = conversation;
    thread(conversation.id);
    notifyListeners();
    if (conversation.unread) setRead(conversation, read: true);
  }

  void close() {
    if (opened != null) {
      opened = null;
    } else if (openBundle != null) {
      openBundle = null;
    } else {
      return;
    }
    notifyListeners();
  }

  /// The thread's messages: loaded once and kept while the app runs. Resting on a row starts it, so
  /// opening renders with the bodies already in.
  ThreadEntry thread(String id) {
    final existing = _threads[id];
    if (existing != null && existing.error == null) return existing;
    late final ThreadEntry entry;
    entry = ThreadEntry(() async {
      try {
        final result = await _show(id);
        final messages = (result['messages'] as List).map((m) => MessageDetail.fromJson(m as Map<String, Object?>)).toList();
        entry.messages = messages;
        return messages;
      } on ControlException catch (error) {
        entry.error = error.message;
        rethrow;
      } finally {
        notifyListeners();
      }
    }());
    entry.future.ignore();
    return _threads[id] = entry;
  }

  Future<Map<String, Object?>> _show(String id) async {
    final args = <String, Object?>{'id': id, 'max-chars': 400000};
    if (_htmlBodies) {
      try {
        return await _call('mail.show', {...args, 'html': true});
      } on ControlException catch (error) {
        if (error.code != 'usage' || !error.message.contains('html')) rethrow;
        _htmlBodies = false;
      }
    }
    return await _call('mail.show', args);
  }

  /// A command on conversation ids. The app remembers ids only while it runs: after it restarts,
  /// the list in view is fetched again, which registers them anew, and the command runs once more.
  Future<Map<String, Object?>> _call(String command, Map<String, Object?> args) async {
    for (var attempt = 0;; attempt++) {
      try {
        final result = await client.call(command, args) as Map<String, Object?>;
        final failed = ((result['failed'] as List?) ?? const []).cast<Map<String, Object?>>();
        if (attempt == 0 && failed.any(_unknownID)) {
          await _reregister();
          continue;
        }
        return result;
      } on ControlException catch (error) {
        if (attempt > 0 || error.code != 'unknown_conversation') rethrow;
        await _reregister();
      }
    }
  }

  /// `mail.tag` reports failures without codes.
  static bool _unknownID(Map<String, Object?> failure) =>
      failure['code'] == 'unknown_conversation' || (failure['message'] as String? ?? '').startsWith('Unknown conversation');

  Future<void> _reregister() async {
    final query = searchQuery;
    if (query != null) {
      try {
        await client.call('mail.search', {'query': query, 'limit': 60});
      } on ControlException {
        // The retry reports the failure.
      }
    }
    await _fetch(selection);
  }

  void prefetch(Conversation conversation) => thread(conversation.id);

  // MARK: - Actions

  /// Every list's copy of `id`, replaced; the reader's too.
  void _patch(String id, Conversation Function(Conversation) change) {
    for (final entry in [..._lists.values, _search]) {
      final index = entry.items.indexWhere((c) => c.id == id);
      if (index >= 0) entry.items = [...entry.items]..[index] = change(entry.items[index]);
    }
    if (opened?.id == id) opened = change(opened!);
    notifyListeners();
  }

  /// Takes `id` out of every list; returns a function that puts it back where it was.
  VoidCallback _remove(String id) {
    final removed = <ListEntry, (int, Conversation)>{};
    for (final entry in [..._lists.values, _search]) {
      final index = entry.items.indexWhere((c) => c.id == id);
      if (index < 0) continue;
      removed[entry] = (index, entry.items[index]);
      entry.items = [...entry.items]..removeAt(index);
    }
    if (opened?.id == id) opened = null;
    notifyListeners();
    return () {
      for (final MapEntry(key: entry, value: (index, conversation)) in removed.entries) {
        if (entry.items.any((c) => c.id == id)) continue;
        entry.items = [...entry.items]..insert(index.clamp(0, entry.items.length), conversation);
      }
      notifyListeners();
    };
  }

  /// Runs a batch command on one id: `{"done": […], "failed": [{id, message}]}`.
  Future<void> _command(String command, Conversation c, Map<String, Object?> args, VoidCallback undo, String verb) async {
    try {
      final result = await _call(command, {'ids': [c.id], ...args});
      final failed = (result['failed'] as List?) ?? const [];
      if (failed.isNotEmpty) {
        undo();
        _report('Could not $verb: ${(failed.first as Map)['message']}');
      } else {
        unawaited(reloadAccounts());
      }
    } on ControlException catch (error) {
      undo();
      _report('Could not $verb: ${error.message}');
    }
  }

  void archive(Conversation c) => _command('mail.archive', c, const {}, _remove(c.id), 'archive');

  void trash(Conversation c) => _command('mail.trash', c, const {}, _remove(c.id), 'move to the trash');

  void setRead(Conversation c, {required bool read}) {
    _patch(c.id, (x) => x.copyWith(unread: !read));
    _command('mail.read', c, {'unread': !read}, () => _patch(c.id, (x) => x.copyWith(unread: read)), read ? 'mark read' : 'mark unread');
  }

  void setStarred(Conversation c, {required bool starred}) {
    _patch(c.id, (x) => x.copyWith(starred: starred));
    _command('mail.star', c, {'off': !starred}, () => _patch(c.id, (x) => x.copyWith(starred: !starred)), starred ? 'star' : 'unstar');
  }

  /// Clears "waiting on you": the user has dealt with it.
  void resolveWaiting(Conversation c) {
    _patch(c.id, (x) => x.copyWith(waiting: false));
    _command('mail.tag', c, {'waiting': 'false'}, () => _patch(c.id, (x) => x.copyWith(waiting: true)), 'update the tag');
  }

  /// Sends a plain-text reply; the app plans the recipients and quotes the original. The Send
  /// button is the person's confirmation, hence `yes`.
  Future<bool> reply(Conversation c, String body, {required bool all}) async {
    try {
      final result = await _call('mail.reply', {'id': c.id, 'body': body, 'all': all, 'yes': true});
      final to = (result['to'] as List?)?.cast<String>().map((a) => Address.parse(a).displayName).join(', ') ?? '';
      showToast('Sent to $to');
      return true;
    } on ControlException catch (error) {
      _report('Could not send: ${error.message}');
      return false;
    }
  }

  void openFile(String path) => Process.run('open', [path]);

  // MARK: - Toasts

  void showToast(String text, {bool error = false}) {
    toast = Toast(text, error: error);
    _toastTimer?.cancel();
    _toastTimer = Timer(Duration(seconds: error ? 6 : 3), () {
      toast = null;
      notifyListeners();
    });
    notifyListeners();
  }

  void _report(String message) => showToast(message, error: true);
}
