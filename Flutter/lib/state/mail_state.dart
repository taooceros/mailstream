import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart';
import 'chat_state.dart';

/// What the list shows: one role across every account, or one account's mailbox.
sealed class Selection {
  const Selection();

  String get key;
  MailboxRole get role;

  /// The engine's listing of it; `cached` reads what the engine holds without fetching.
  engine.ListQuery query({required int limit, String? cursor, bool cached = false});
}

final class UnifiedSelection extends Selection {
  const UnifiedSelection(this.role);

  @override
  final MailboxRole role;

  @override
  String get key => 'unified:${role.name}';

  @override
  engine.ListQuery query({required int limit, String? cursor, bool cached = false}) =>
      engine.ListQuery(mailboxes: [role.name], limit: limit, cursor: cursor, cached: cached);

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
  engine.ListQuery query({required int limit, String? cursor, bool cached = false}) =>
      engine.ListQuery(mailboxes: [mailbox.id], account: mailbox.account, limit: limit, cursor: cursor, cached: cached);

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

  /// `mail.list`'s cursor for the page after `items`; nil at the end.
  String? next;
  bool loadingMore = false;

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

/// A message for a snack bar; each one shows once.
class Toast {
  Toast(this.text, {this.error = false});

  final String text;
  final bool error;
}

/// The engine's change events, handed to the state.
class _Changes implements engine.EventListener {
  _Changes(this.deliver);

  final void Function(engine.Event) deliver;

  @override
  void onEvent(engine.Change change) => deliver(change.event);
}

/// What a failed engine call says.
String describe(Object error) => error is engine.FailedMailException ? error.message : '$error';

/// The UI's single source of truth. Mailstream's engine runs in this process (`package:mailstream`)
/// and owns accounts, sync, caching, threading and tagging; this keeps what the window shows,
/// reads it again when the engine reports a change, and applies the user's actions optimistically,
/// rolling them back when the engine reports a failure.
class MailState extends ChangeNotifier {
  MailState(this.mail);

  final engine.Mailstream mail;

  /// A list not in view fetched within this long serves reads without asking again; the one in
  /// view follows the engine's change events.
  static const listLifetime = Duration(seconds: 90);

  /// Conversations per page, the engine's most. Long pages matter in the Smart Inbox: most of a
  /// page folds into bundles, so short ones load one after another near the list's end.
  static const pageSize = 200;

  List<Account> accounts = const [];
  Map<String, List<Mailbox>> mailboxes = const {};
  Selection selection = const UnifiedSelection(MailboxRole.inbox);

  /// Google Chat, which the window shows instead of mail while `chat.active`.
  late final ChatState chat = ChatState(mail, report: _report);

  /// Set while the Smart Inbox bundle of that category lies over the list.
  MailCategory? openBundle;

  /// The conversation in the reader.
  Conversation? opened;

  String? searchQuery;
  final ListEntry _search = ListEntry();
  Timer? _searchDebounce;

  Toast? toast;

  final Map<String, ListEntry> _lists = {};
  final Map<String, ThreadEntry> _threads = {};

  /// Change events gathered for one re-read: the engine sends bursts while it syncs.
  Timer? _changes;
  bool _accountsChanged = false;
  bool _listsChanged = false;

  /// Conversations whose preview `previews` is loading or has answered.
  final Set<String> _previewRequested = {};

  /// Previews waiting for the request in flight to finish.
  final Set<String> _previewQueue = {};
  bool _previewsRunning = false;

  /// Sender address → the file of their picture in the engine's cache, or null when they have
  /// none. An address absent here has not been asked for yet.
  final Map<String, String?> _avatars = {};

  /// Senders (and whether they are automated) waiting for the request in flight to finish.
  final Map<String, bool> _avatarQueue = {};
  bool _avatarsRunning = false;

  /// Senders asked for only for conversations folded into a closed bundle: their pictures
  /// arrive without a rebuild.
  final Set<String> _foldedSenders = {};

  /// The picture for `email`, once the engine found one.
  String? avatar(String email) => _avatars[email.toLowerCase()];

  /// Inline reply drafts by conversation, kept until sent or discarded (in memory, like the app).
  final Map<String, String> drafts = {};

  // MARK: - Startup

  Future<void> start() async {
    mail.subscribe(listener: _Changes(_changed));
    await reloadAccounts();
    unawaited(reloadSettings());
    unawaited(chat.reload());
    await _fetch(selection);
  }

  @override
  void dispose() {
    _changes?.cancel();
    _searchDebounce?.cancel();
    super.dispose();
  }

  void _changed(engine.Event event) {
    chat.changed(event);
    switch (event) {
      case engine.AccountsEvent() || engine.MailboxesEvent():
        _accountsChanged = true;
      case engine.ConversationsEvent() || engine.TagsEvent():
        _listsChanged = true;
      case engine.SettingsEvent():
        unawaited(reloadSettings());
        return;
      default:
        return;
    }
    _changes ??= Timer(const Duration(milliseconds: 400), () {
      _changes = null;
      if (_accountsChanged) unawaited(reloadAccounts());
      if (_listsChanged) {
        // Lists out of view are read again when they come back into view.
        for (final MapEntry(:key, value: entry) in _lists.entries) {
          if (key != selection.key) entry.fetchedAt = null;
        }
        unawaited(_fetch(selection, quiet: true));
      }
      _accountsChanged = _listsChanged = false;
    });
  }

  Future<void> reloadAccounts() async {
    try {
      final list = await mail.accounts();
      final boxes = await mail.mailboxes(account: null);
      accounts = list.map(Account.of).toList();
      mailboxes = {
        for (final MapEntry(key: account, value: boxes) in boxes.entries)
          account: [for (final box in boxes) Mailbox.of(account, box)],
      };
    } on engine.MailException catch (error) {
      _report(describe(error));
    }
    notifyListeners();
  }

  /// Fetches new mail and counts now; the engine's change events bring them in.
  Future<void> refresh() async {
    try {
      await mail.refresh();
    } on engine.MailException catch (error) {
      _report('Could not refresh: ${describe(error)}');
    }
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

  /// A first page is loading (the bar over the list). An older page shows the list's footer
  /// spinner instead, so loading one rebuilds nothing until it lands.
  bool get isLoading => _current.loading;
  String? get listError => _current.error;

  /// Whether the list in view has older pages to load.
  bool get canLoadMore => searchQuery == null && _current.next != null;

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
    chat.hide();
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

  static List<Conversation> _decode(engine.Listing listing) => listing.conversations.map(Conversation.of).toList();

  /// The first page. A refresh keeps the older pages already loaded below it: their cursor still
  /// points past them. `quiet` (a change the engine reported) shows no progress, reads what the
  /// engine holds rather than asking the providers again, and leaves the window alone when
  /// nothing it shows changed.
  Future<void> _fetch(Selection target, {bool quiet = false}) async {
    final entry = _lists[target.key] ??= ListEntry();
    final generation = ++entry.generation;
    if (!quiet) {
      entry.loading = true;
      notifyListeners();
    }
    var changed = !quiet;
    try {
      final listing = await mail.list(query: target.query(limit: pageSize, cached: quiet));
      if (generation != entry.generation) return;
      final known = {for (final c in entry.items) c.id: c};
      final page = [for (final c in _decode(listing)) _keep(known[c.id], c)];
      final ids = {for (final c in page) c.id};
      final oldest = page.lastOrNull?.date;
      final kept = oldest == null
          ? const <Conversation>[]
          : entry.items.where((c) => !ids.contains(c.id) && c.date != null && c.date!.isBefore(oldest)).toList();
      final items = [...page, ...kept];
      final next = kept.isEmpty ? listing.next : entry.next;
      final error = listing.failures.isEmpty ? null : listing.failures.join('\n');
      changed |= next != entry.next || error != entry.error || !_sameRows(items, entry.items);
      if (changed) {
        entry
          ..items = items
          ..next = next
          ..error = error;
      }
      entry.fetchedAt = DateTime.now();
      _loadPreviews(page);
      _loadAvatars(page);
    } on engine.MailException catch (error) {
      if (generation != entry.generation) return;
      changed |= entry.error != describe(error);
      entry.error = describe(error);
    } finally {
      if (generation == entry.generation) {
        entry.loading = false;
        if (changed) notifyListeners();
      }
    }
  }

  /// `fresh`, with the preview `old` has when the listing lacks one; `old` itself when that
  /// shows the same, so the row keeps its instance.
  static Conversation _keep(Conversation? old, Conversation fresh) {
    if (old == null) return fresh;
    final filled = fresh.snippet == null && old.snippet != null ? fresh.copyWith(snippet: old.snippet) : fresh;
    return old.sameAs(filled) ? old : filled;
  }

  static bool _sameRows(List<Conversation> a, List<Conversation> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (!identical(a[i], b[i])) return false;
    }
    return true;
  }

  /// The page after the list in view: scrolling near its end, or a Smart Inbox that folds a page
  /// into bundles and still leaves the window empty.
  Future<void> loadMore() async {
    if (searchQuery != null) return;
    final entry = _current;
    final cursor = entry.next;
    if (cursor == null || entry.loadingMore || entry.loading) return;
    final generation = entry.generation;
    entry.loadingMore = true;
    try {
      final listing = await mail.list(query: selection.query(limit: pageSize, cursor: cursor));
      // A first page landed meanwhile and decided the cursor.
      if (generation != entry.generation || entry.next != cursor) return;
      final ids = {for (final c in entry.items) c.id};
      final page = _decode(listing).where((c) => !ids.contains(c.id)).toList();
      entry
        ..items = [...entry.items, ...page]
        ..next = listing.next;
      _loadPreviews(page);
      _loadAvatars(page);
    } on engine.MailException catch (error) {
      _report('Could not load more: ${describe(error)}');
    } finally {
      entry.loadingMore = false;
      notifyListeners();
    }
  }

  /// Previews of conversations listed without one: the engine loads their latest messages.
  /// Ids asked for while a request runs go together in the next one.
  void _loadPreviews(List<Conversation> conversations) {
    for (final c in conversations) {
      if ((c.snippet == null || c.snippet!.isEmpty) && _previewRequested.add(c.id)) _previewQueue.add(c.id);
    }
    unawaited(_sendPreviews());
  }

  Future<void> _sendPreviews() async {
    if (_previewsRunning || _previewQueue.isEmpty) return;
    final ids = _previewQueue.toList();
    _previewQueue.clear();
    _previewsRunning = true;
    try {
      final previews = await mail.previews(ids: ids);
      final changes = {
        for (final MapEntry(key: id, value: snippet) in previews.entries)
          if (snippet != null && snippet.isNotEmpty) id: (Conversation c) => c.copyWith(snippet: snippet),
      };
      // A bundle's row shows no previews: ones only for conversations folded into a closed
      // bundle are kept without rebuilding the window (pages loaded near the list's end often
      // fold whole into bundles).
      final shown = opened != null && changes.containsKey(opened!.id) ||
          conversations.any((c) => changes.containsKey(c.id) && !_folded(c));
      _patchAll(changes, rebuild: shown);
    } on engine.MailException {
      // Rows keep their subject alone; a later listing asks again.
      _previewRequested.removeAll(ids);
    } finally {
      _previewsRunning = false;
    }
    await _sendPreviews();
  }

  /// Whether `c` sits folded in a closed Smart Inbox bundle, which shows none of its preview or
  /// sender picture.
  bool _folded(Conversation c) => bundlesEnabled && c.bundle != null && c.bundle != openBundle;

  /// Pictures of the senders `conversations` show, from the engine's avatar cache (it looks up
  /// the ones it has not cached yet).
  void _loadAvatars(List<Conversation> conversations) {
    final shown = {for (final c in conversations) if (c.from != null && !_folded(c)) c.from!.email.toLowerCase()};
    _foldedSenders.removeAll(shown);
    for (final c in conversations) {
      final key = c.from?.email.toLowerCase();
      if (key != null && !shown.contains(key) && !_avatars.containsKey(key)) _foldedSenders.add(key);
    }
    _queueAvatars([
      for (final c in conversations)
        if (c.from != null) (c.from!.email, c.category != null && c.category != MailCategory.people),
    ]);
  }

  /// Asks the engine for the pictures of `senders` not asked for yet; automated senders may take
  /// their site's icon. Senders asked for while a request runs go together in the next one.
  void requestAvatars(Iterable<(String, bool)> senders) {
    _foldedSenders.removeAll([for (final (email, _) in senders) email.toLowerCase()]);
    _queueAvatars(senders);
  }

  void _queueAvatars(Iterable<(String, bool)> senders) {
    for (final (email, isAutomated) in senders) {
      final key = email.toLowerCase();
      if (!key.contains('@') || _avatars.containsKey(key)) continue;
      _avatars[key] = null;
      _avatarQueue[key] = isAutomated;
    }
    unawaited(_sendAvatars());
  }

  Future<void> _sendAvatars() async {
    if (_avatarsRunning || _avatarQueue.isEmpty) return;
    final batch = Map.of(_avatarQueue);
    _avatarQueue.clear();
    _avatarsRunning = true;
    try {
      final pictures = await mail.avatars(
        addresses: batch.keys.toList(),
        automated: [for (final MapEntry(:key, :value) in batch.entries) if (value) key],
      );
      // Pictures only of senders folded into closed bundles are kept without a rebuild.
      var shown = false;
      for (final MapEntry(key: email, value: url) in pictures.entries) {
        if (url == null) continue;
        _avatars[email] = Uri.parse(url).toFilePath();
        shown |= !_foldedSenders.contains(email);
      }
      _foldedSenders.removeAll(batch.keys);
      if (shown) notifyListeners();
    } on engine.MailException {
      // Initials stay; the next listing asks again.
      batch.keys.forEach(_avatars.remove);
    } finally {
      _avatarsRunning = false;
    }
    await _sendAvatars();
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
        final listing = await mail.search(query: text, account: null, limit: 60);
        if (generation != _search.generation) return;
        final found = _decode(listing);
        _search
          ..items = found
          ..fetchedAt = DateTime.now();
        _loadPreviews(found);
        _loadAvatars(found);
      } on engine.MailException catch (error) {
        if (generation != _search.generation) return;
        _search.error = describe(error);
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
        final thread = await mail.show_(id: id, maxChars: 400000, html: true);
        final messages = thread.messages.map(MessageDetail.of).toList();
        entry.messages = messages;
        requestAvatars([for (final m in messages) (m.sender.email, false)]);
        return messages;
      } on engine.MailException catch (error) {
        entry.error = describe(error);
        rethrow;
      } finally {
        notifyListeners();
      }
    }());
    entry.future.ignore();
    return _threads[id] = entry;
  }

  void prefetch(Conversation conversation) => thread(conversation.id);

  // MARK: - Actions

  /// Every list's copy of `id`, replaced; the reader's too.
  void _patch(String id, Conversation Function(Conversation) change) => _patchAll({id: change});

  /// Each changed conversation replaced in every list and the reader, in one pass and one rebuild
  /// (none with `rebuild: false`, for changes the window does not show).
  void _patchAll(Map<String, Conversation Function(Conversation)> changes, {bool rebuild = true}) {
    if (changes.isEmpty) return;
    for (final entry in [..._lists.values, _search]) {
      if (!entry.items.any((c) => changes.containsKey(c.id))) continue;
      entry.items = [for (final c in entry.items) changes[c.id]?.call(c) ?? c];
    }
    if (opened case final open?) opened = changes[open.id]?.call(open) ?? open;
    if (rebuild) notifyListeners();
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

  /// Runs an action on one conversation; a failure puts the window back with `undo`.
  Future<void> _command(
    Conversation c,
    Future<List<engine.BatchFailure>> Function(List<String> ids) action,
    VoidCallback undo,
    String verb,
  ) async {
    try {
      final failed = await action([c.id]);
      if (failed.isNotEmpty) {
        undo();
        _report('Could not $verb: ${failed.first.message}');
      }
    } on engine.MailException catch (error) {
      undo();
      _report('Could not $verb: ${describe(error)}');
    }
  }

  void archive(Conversation c) =>
      _command(c, (ids) async => (await mail.archive(ids: ids)).failed, _remove(c.id), 'archive');

  /// Moves to the trash; conversations already there stay, as deleting for good needs a confirmation.
  void trash(Conversation c) =>
      _command(c, (ids) async => (await mail.trash(ids: ids, confirm: false)).failed, _remove(c.id), 'move to the trash');

  void setRead(Conversation c, {required bool read}) {
    _patch(c.id, (x) => x.copyWith(unread: !read));
    _command(
      c,
      (ids) async => (await mail.setRead(ids: ids, read: read)).failed,
      () => _patch(c.id, (x) => x.copyWith(unread: read)),
      read ? 'mark read' : 'mark unread',
    );
  }

  void setStarred(Conversation c, {required bool starred}) {
    _patch(c.id, (x) => x.copyWith(starred: starred));
    _command(
      c,
      (ids) async => (await mail.setStarred(ids: ids, starred: starred)).failed,
      () => _patch(c.id, (x) => x.copyWith(starred: !starred)),
      starred ? 'star' : 'unstar',
    );
  }

  /// Resolves "Waiting on you" (`waiting: false`: the user has dealt with it) or puts the
  /// conversation back on it.
  void setWaiting(Conversation c, {required bool waiting}) {
    _patch(c.id, (x) => x.copyWith(waiting: waiting));
    _command(
      c,
      (ids) async => (await mail.tag(ids: ids, category: null, waiting: waiting, labels: null)).failed,
      () => _patch(c.id, (x) => x.copyWith(waiting: !waiting)),
      'update the tag',
    );
  }

  /// Sends a plain-text reply; the engine plans the recipients and quotes the original. The Send
  /// button is the person's confirmation.
  Future<bool> reply(Conversation c, String body, {required bool all}) async {
    try {
      _sentToast(await mail.reply(id: c.id, body: body, all: all, confirm: true));
      return true;
    } on engine.MailException catch (error) {
      _report('Could not send: ${describe(error)}');
      return false;
    }
  }

  /// Sends a new plain-text message (`mail.send`); the Send button is the person's confirmation.
  /// Throws the engine's refusal (an unknown account, an address it does not take, the
  /// provider's error) for the compose window to show.
  Future<void> send(engine.Draft draft) async => _sentToast(await mail.send(draft: draft, confirm: true));

  void _sentToast(engine.Sent sent) => showToast('Sent to ${sent.to.map((a) => Address.parse(a).displayName).join(', ')}');

  /// Opens a file in its app, or a link in the browser.
  void openExternal(String pathOrUrl) => Process.run('open', [pathOrUrl]);

  // MARK: - Settings and accounts

  /// settings.json as the engine reads it: the one file Mailstream.app, the CLI and this app
  /// share. Null until first read.
  engine.Settings? settings;

  Future<void> reloadSettings() async {
    try {
      settings = await mail.settings();
    } on engine.MailException catch (error) {
      _report('Could not read settings: ${describe(error)}');
    }
    notifyListeners();
  }

  /// Writes one setting (null resets it to its default) and shows the value at once; throws the
  /// engine's refusal (out of range, not an address) for the field to show.
  Future<void> setSetting(String key, engine.SettingValue? value) async {
    final entry = value == null ? await mail.resetSetting(key: key) : await mail.setSetting(key: key, value: value);
    final current = settings;
    if (current == null) return;
    settings = engine.Settings(
      file: current.file,
      settings: [for (final e in current.settings) e.key == key ? entry : e],
      issues: current.issues,
    );
    notifyListeners();
  }

  Future<void> renameAccount(String account, String label) async {
    try {
      await mail.renameAccount(account: account, label: label);
    } on engine.MailException catch (error) {
      _report('Could not rename: ${describe(error)}');
    }
    await reloadAccounts();
  }

  /// Signs out of the account; the person confirmed in the dialog.
  Future<void> removeAccount(String account) async {
    try {
      await mail.removeAccount(account: account, confirm: true);
    } on engine.MailException catch (error) {
      _report('Could not remove $account: ${describe(error)}');
    }
    await reloadAccounts();
  }

  /// Browser sign-in; throws the engine's reason when it fails or times out.
  Future<Account> addAccount(String provider, {String? clientId, String? clientSecret}) async {
    final record = await mail.addAccount(provider: provider, clientId: clientId, clientSecret: clientSecret);
    await reloadAccounts();
    return Account.of(record);
  }

  // MARK: - Toasts

  void showToast(String text, {bool error = false}) {
    toast = Toast(text, error: error);
    notifyListeners();
  }

  void _report(String message) => showToast(message, error: true);
}
