import 'dart:async';

import 'package:mailstream/mailstream.dart' as engine;

/// Synthetic mail only. Delayed body answers exercise the same event/list/show boundary as FFI.
class ReaderMail extends FakeMailstream {
  late engine.EventListener listener;
  List<engine.Conversation> rows = [conversation('a'), conversation('b')];
  final queries = <engine.ListQuery>[];
  final requests = <({String id, Completer<engine.Thread> answer})>[];

  void changed([engine.Event? event]) => listener.onEvent(engine.Change(event: event ?? engine.ConversationsEvent()));

  @override
  void subscribe({required engine.EventListener listener}) => this.listener = listener;

  @override
  Future<List<engine.Account>> accounts() async => [
    engine.Account(id: 'reader@example.test', label: 'Reader regression', provider: engine.Provider.google, services: ['mail']),
  ];

  @override
  Future<Map<String, List<engine.Mailbox>>> mailboxes({required String? account}) async => {
    'reader@example.test': [engine.Mailbox(id: 'INBOX', name: 'Inbox', role: engine.MailboxRole.inbox, total: 2, unread: 0)],
  };

  @override
  Future<engine.Settings> settings() async => engine.Settings(file: '', settings: [], issues: []);

  @override
  Future<engine.Status> status() async => engine.Status(
    version: 'test',
    pid: 0,
    settingsFile: '',
    settingsIssues: [],
    accounts: [],
    sync_: engine.SyncStatus(loading: false, lastRefresh: null, lastError: null, pushConnected: false),
    aiTagging: engine.TaggingStatus(enabled: false, status: null, failed: false),
  );

  @override
  Future<engine.ChatSpaces> chatSpaces({required String? account}) async =>
      engine.ChatSpaces(accounts: [], spaces: [], loading: false, more: false);

  @override
  Future<engine.CalendarAgenda> calendarEvents({required String? from, required int days}) async => engine.CalendarAgenda(
    accounts: [],
    loading: false,
    events: [],
    timeZone: 'UTC',
    synced: engine.CalendarDays(from: '2026-11-01', until: '2027-01-08'),
  );

  @override
  Future<Map<String, String?>> avatars({required List<String> addresses, required List<String> automated}) async => {};

  /// Auto-read's log as the engine answers it; [undoAutoRead] drops what it puts back and, as
  /// the engine does, sends the autoRead event.
  engine.AutoReadLog autoRead = engine.AutoReadLog(enabled: false, days: 3, entries: []);
  final undone = <List<String>?>[];

  @override
  Future<engine.AutoReadLog> autoReadLog() async => autoRead;

  @override
  Future<engine.BatchResult> undoAutoRead({required List<String>? ids}) async {
    undone.add(ids);
    final back = ids ?? [for (final e in autoRead.entries) e.id];
    autoRead = engine.AutoReadLog(
      enabled: autoRead.enabled,
      days: autoRead.days,
      entries: [for (final e in autoRead.entries) if (!back.contains(e.id)) e],
    );
    changed(engine.AutoReadEvent());
    return engine.BatchResult(done: back, failed: []);
  }

  @override
  Future<engine.Listing> list({required engine.ListQuery query}) async {
    queries.add(query);
    return engine.Listing(conversations: rows, failures: []);
  }

  @override
  Future<engine.Thread> show_({required String id, required int maxChars, required bool html}) {
    final answer = Completer<engine.Thread>();
    requests.add((id: id, answer: answer));
    return answer.future;
  }

  void answer(int index, engine.Conversation c) => requests[index].answer.complete(engine.Thread(
    conversation: c,
    messages: [for (var i = 1; i <= c.messageCount; i++) message(c.id, i)],
  ));
}

// Implementing the generated class avoids constructing an FFI host in consumer tests.
class FakeMailstream implements engine.Mailstream {
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

engine.Conversation conversation(String id, {int count = 1, int? minute, bool starred = false}) => engine.Conversation(
  id: id,
  account: 'reader@example.test',
  subject: id == 'a' ? 'Live reader update' : 'Another conversation',
  date: DateTime.utc(2026, 11, 6, 12, minute ?? count).toIso8601String(),
  avatar: engine.avatar(name: 'Alex', key: 'alex@example.test'),
  from: engine.Address(name: 'Alex', email: 'alex@example.test'),
  participants: ['Alex'],
  messageCount: count,
  unread: false,
  starred: starred,
  hasAttachment: false,
  mailboxes: [engine.MailboxRole.inbox],
  snippet: 'Synthetic message $count',
  category: engine.Category.people,
  waiting: false,
  labels: [],
);

engine.Message message(String id, int number) => engine.Message(
  id: '$id-$number',
  mailbox: 'INBOX',
  role: engine.MailboxRole.inbox,
  avatar: engine.avatar(name: 'Alex', key: 'alex@example.test'),
  from: [engine.Address(name: 'Alex', email: 'alex@example.test')],
  to: [engine.Address(email: 'reader@example.test')],
  cc: [],
  date: DateTime.utc(2026, 11, 6, 12, number).toIso8601String(),
  subject: id == 'a' ? 'Live reader update' : 'Another conversation',
  unread: false,
  starred: false,
  text: id == 'b' ? 'Selected conversation B stays open.' : number == 1
      ? 'First message: keep this conversation open.'
      : 'New message $number arrived. The reader updated without reopening.',
  truncated: false,
  attachments: [],
);
