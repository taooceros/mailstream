import 'dart:async';

import 'package:flutter/foundation.dart';

import 'package:mailstream/mailstream.dart' as engine;

/// One space, by account and `spaces/…` name.
typedef SpaceKey = ({String account, String space});

SpaceKey spaceKey(engine.ChatSpace space) => (account: space.account, space: space.space);

/// Google Chat in the window: the chat accounts' spaces and the open space's messages. The engine
/// (`ChatStore`) polls Google, caches spaces and recently opened histories on disk and sends
/// optimistically; this keeps what it last answered and reads it again on its `chat` and
/// `chatMessages` events, so what is loaded shows at once and nothing waits on the network.
class ChatState extends ChangeNotifier {
  ChatState(this.mail, {required this.report});

  final engine.Mailstream mail;

  /// Shows an error in the window's snack bar.
  final void Function(String message) report;

  /// Chat is in view instead of mail.
  bool active = false;

  List<engine.ChatAccount> accounts = const [];
  List<engine.ChatSpace> spaces = const [];
  bool loading = false;
  bool more = false;

  /// Whether `chat.spaces` answered once: until then an empty list is not "no chats".
  bool listed = false;

  /// The space in the reader.
  SpaceKey? opened;

  /// The loaded history of spaces opened this session; the open one follows the engine.
  final Map<SpaceKey, engine.ChatThread> _threads = {};

  Timer? _spacesChange;
  bool _spacesRunning = false, _spacesAgain = false;
  bool _threadRunning = false, _threadAgain = false;
  bool _moreRunning = false;

  engine.ChatThread? thread(SpaceKey key) => _threads[key];

  engine.ChatSpace? space(SpaceKey key) =>
      spaces.where((s) => s.account == key.account && s.space == key.space).firstOrNull;

  engine.ChatSpace? get openedSpace => opened == null ? null : space(opened!);

  int get unreadSpaces => spaces.where((s) => s.unread).length;

  /// Called for each engine event.
  void changed(engine.Event event) {
    switch (event) {
      case engine.ChatEvent():
        // Bursts while a poll lands: one read per burst.
        _spacesChange ??= Timer(const Duration(milliseconds: 150), () {
          _spacesChange = null;
          unawaited(reload());
        });
      case engine.ChatMessagesEvent():
        unawaited(_reloadThread());
      default:
        return;
    }
  }

  @override
  void dispose() {
    _spacesChange?.cancel();
    super.dispose();
  }

  /// Reads the spaces the engine holds; a request made meanwhile runs once after it.
  Future<void> reload() async {
    if (_spacesRunning) {
      _spacesAgain = true;
      return;
    }
    _spacesRunning = true;
    try {
      final answer = await mail.chatSpaces(account: null);
      accounts = answer.accounts;
      spaces = answer.spaces;
      loading = answer.loading;
      more = answer.more;
      listed = true;
      notifyListeners();
    } on engine.MailException catch (error) {
      report('Could not list chats: ${_describe(error)}');
    } finally {
      _spacesRunning = false;
    }
    if (_spacesAgain) {
      _spacesAgain = false;
      await reload();
    }
  }

  /// Chat comes into view: the cached spaces at once, Google's newest after.
  void show() {
    if (!active) {
      active = true;
      notifyListeners();
    }
    unawaited(reload());
    unawaited(_quiet(mail.chatRefresh()));
  }

  /// Mail comes back into view.
  void hide() {
    if (!active) return;
    active = false;
    close();
    notifyListeners();
  }

  Future<void> refresh() => _quiet(mail.chatRefresh());

  /// The next page of older spaces, near the list's end.
  Future<void> loadMore() async {
    if (!more || _moreRunning) return;
    _moreRunning = true;
    try {
      await mail.chatMore(account: null);
    } on engine.MailException catch (error) {
      report('Could not load more chats: ${_describe(error)}');
    } finally {
      _moreRunning = false;
    }
  }

  /// Opens `key` in the reader: what is loaded shows at once, the newest follows by event.
  Future<void> open(SpaceKey key) async {
    opened = key;
    notifyListeners();
    try {
      final thread = await mail.chatOpen(account: key.account, space: key.space);
      _threads[key] = thread;
      if (opened == key) notifyListeners();
    } on engine.MailException catch (error) {
      report('Could not open the chat: ${_describe(error)}');
    }
  }

  void close() {
    if (opened == null) return;
    opened = null;
    notifyListeners();
    unawaited(_quiet(mail.chatClose()));
  }

  Future<void> _reloadThread() async {
    final key = opened;
    if (key == null) return;
    if (_threadRunning) {
      _threadAgain = true;
      return;
    }
    _threadRunning = true;
    try {
      final thread = await mail.chatMessages(account: key.account, space: key.space);
      _threads[key] = thread;
      if (opened == key) notifyListeners();
    } on engine.MailException {
      // The next event reads it again.
    } finally {
      _threadRunning = false;
    }
    if (_threadAgain) {
      _threadAgain = false;
      await _reloadThread();
    }
  }

  Future<void> loadEarlier(SpaceKey key) async {
    final thread = _threads[key];
    if (thread == null || !thread.more || thread.loadingEarlier) return;
    try {
      _threads[key] = await mail.chatEarlier(account: key.account, space: key.space);
      notifyListeners();
    } on engine.MailException catch (error) {
      report('Could not load earlier messages: ${_describe(error)}');
    }
  }

  /// Sends `text`; it shows pending at once (by event) and the call ends once Google took it.
  /// The Send button is the person's confirmation.
  Future<bool> send(SpaceKey key, String text) async {
    try {
      await mail.chatSend(account: key.account, space: key.space, text: text, thread: null, confirm: true);
      return true;
    } on engine.MailException catch (error) {
      report('Could not send: ${_describe(error)}');
      return false;
    }
  }

  Future<engine.ChatPeople> people(String account) => mail.chatPeople(account: account);

  /// Finds or starts the conversation with `emails` and opens it. Throws the engine's refusal for
  /// the dialog to show.
  Future<void> start(String account, List<String> emails) async {
    final found = await mail.chatStart(account: account, people: emails);
    await reload();
    await open((account: found.account, space: found.space));
  }

  Future<void> _quiet(Future<void> call) async {
    try {
      await call;
    } on engine.MailException catch (error) {
      report(_describe(error));
    }
  }

  static String _describe(engine.MailException error) =>
      error is engine.FailedMailException ? error.message : '$error';
}
