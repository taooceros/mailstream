import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/reader.dart';

import 'reader_fixture.dart';

void main() {
  late ReaderMail mail;
  late MailState state;

  Future<void> start({List<engine.Conversation>? rows}) async {
    mail = ReaderMail();
    if (rows != null) mail.rows = rows;
    state = MailState(mail);
    await state.start();
  }

  /// `a`, `b` and enough others to push loaded conversations out of the state's cache.
  List<engine.Conversation> many() => [
    conversation('a'),
    conversation('b'),
    for (var i = 0; i < 2 * MailState.keptThreads + 8; i++) conversation('c$i'),
  ];

  /// Prefetches [c] and answers its body request, as resting on its row does.
  Future<ThreadEntry> prefetched(WidgetTester tester, engine.Conversation c) async {
    final entry = state.prefetch(c);
    final index = mail.requests.lastIndexWhere((r) => r.id == c.id);
    if (!mail.requests[index].answer.isCompleted) mail.answer(index, mail.rows.firstWhere((r) => r.id == c.id));
    await tester.pump();
    return entry;
  }

  Future<void> update(WidgetTester tester, List<engine.Conversation> rows) async {
    mail.rows = rows;
    mail.changed();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.pump();
  }

  Future<void> openA(WidgetTester tester) async {
    state.open(state.conversations.first);
    mail.answer(0, mail.rows.first);
    await tester.pump();
  }

  tearDown(() => state.dispose());

  testWidgets('the mounted reader gets new messages, keeping content and selection during the load', (tester) async {
    await start();
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await openA(tester);
    final readerKey = GlobalKey<ReaderState>();
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: ListenableBuilder(
        listenable: state,
        builder: (context, _) => Reader(key: readerKey, state: state, conversation: state.opened!),
      ),
    ));
    await tester.pump();
    final mountedReader = readerKey.currentState;
    expect(find.textContaining('First message: keep'), findsOneWidget);
    final oldMessages = state.thread(state.opened!).messages;

    final revised = conversation('a', count: 2);
    await update(tester, [revised, mail.rows.last]);
    expect(state.opened!.id, 'a');
    expect(state.opened!.messageCount, 2);
    expect(state.thread(state.opened!).messages, same(oldMessages));
    expect(find.textContaining('First message: keep'), findsOneWidget);
    expect(mail.queries.last.cached, isTrue);
    expect(mail.requests.length, 2);

    var listNotifications = 0;
    state.addListener(() => listNotifications++);
    mail.answer(1, revised);
    await tester.pump();
    await tester.pump();
    expect(find.textContaining('New message 2 arrived'), findsOneWidget);
    expect(readerKey.currentState, same(mountedReader));
    expect(state.opened!.id, 'a');
    expect(listNotifications, 0, reason: 'body arrival must not rebuild the list');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('unchanged, tag and flag events keep body and row caches valid', (tester) async {
    await start();
    await openA(tester);
    final a = state.thread(state.opened!);
    final b = state.conversations.last;
    var notifications = 0;
    state.addListener(() => notifications++);
    mail.changed();
    mail.changed(engine.TagsEvent());
    await tester.pump(const Duration(milliseconds: 400));
    expect(mail.requests.length, 1);
    expect(state.thread(state.opened!), same(a));
    expect(state.conversations.last, same(b));
    expect(notifications, 0);
    await update(tester, [conversation('a', starred: true), mail.rows.last]);
    expect(state.opened!.starred, isTrue);
    expect(state.thread(state.opened!), same(a));
    expect(mail.requests.length, 1);
  });

  testWidgets('a changed prefetched thread is fetched lazily on open, unrelated threads remain valid', (tester) async {
    await start();
    state.prefetch(state.conversations.first);
    state.prefetch(state.conversations.last);
    mail.answer(0, mail.rows.first);
    mail.answer(1, mail.rows.last);
    await tester.pump();
    final b = state.thread(state.conversations.last);
    final revised = conversation('a', count: 2);
    await update(tester, [revised, mail.rows.last]);
    expect(mail.requests.length, 2);
    expect(state.thread(state.conversations.last), same(b));
    state.open(state.conversations.first);
    expect(mail.requests.length, 3);
    mail.answer(2, revised);
    await tester.pump();
    expect(state.thread(state.opened!).messages!.length, 2);
  });

  testWidgets('a superseded show answer cannot overwrite a newer revision', (tester) async {
    await start();
    state.open(state.conversations.first);
    final obsolete = state.thread(state.opened!);
    final revision2 = conversation('a', count: 2);
    await update(tester, [revision2, mail.rows.last]);
    final revision3 = conversation('a', count: 3);
    await update(tester, [revision3, mail.rows.last]);
    final current = state.thread(state.opened!);
    mail.answer(2, revision3);
    await tester.pump();
    mail.answer(1, revision2);
    mail.answer(0, conversation('a'));
    await tester.pump();
    expect(state.thread(state.opened!), same(current));
    expect(current.messages!.length, 3);
    expect(obsolete.messages, isNull);
    expect(state.opened!.messageCount, 3);
  });

  testWidgets('switching conversations ignores a late update for the previous reader', (tester) async {
    await start();
    await openA(tester);
    final revision = conversation('a', count: 2);
    await update(tester, [revision, mail.rows.last]);
    state.open(state.conversations.last);
    mail.answer(2, mail.rows.last);
    await tester.pump();
    final selected = state.opened;
    var notifications = 0;
    state.addListener(() => notifications++);
    mail.answer(1, revision);
    await tester.pump();
    expect(state.opened, same(selected));
    expect(state.thread(selected!).messages!.single.id, 'b-1');
    expect(notifications, 0);
  });

  testWidgets('same-count latest-date changes refresh, failures keep old messages until reopening retries', (tester) async {
    await start();
    await openA(tester);
    final revised = conversation('a', minute: 5);
    await update(tester, [revised, mail.rows.last]);
    final failed = state.thread(state.opened!);
    mail.requests[1].answer.completeError(engine.FailedMailException(code: 'offline', message: 'Offline'));
    await tester.pump();
    expect(failed.messages!.single.id, 'a-1');
    expect(failed.error, 'Offline');
    expect(state.thread(state.opened!), same(failed));
    expect(mail.requests.length, 2);
    state.close();
    state.open(state.conversations.first);
    expect(mail.requests.length, 3);
    mail.answer(2, revised);
    await tester.pump();
    expect(state.thread(state.opened!).error, isNull);
  });

  testWidgets('bodies past keptThreads drop out least recently used first; the open one stays', (tester) async {
    await start(rows: many());
    await openA(tester);
    final a = state.thread(state.opened!);
    final others = state.conversations.where((c) => c.id != 'a').toList();
    final first = await prefetched(tester, others[0]);
    final second = await prefetched(tester, others[1]);
    for (final c in others.skip(2).take(MailState.keptThreads - 3)) {
      await prefetched(tester, c);
    }
    // Opening or prefetching again counts as use: the second, not the first, is now the oldest
    // after the open conversation.
    expect(state.thread(others[0]), same(first));
    await prefetched(tester, others[MailState.keptThreads - 1]);
    final requests = mail.requests.length;
    expect(state.thread(state.opened!), same(a), reason: 'the open conversation is never dropped');
    expect(state.thread(others[0]), same(first));
    expect(mail.requests.length, requests);
    expect(state.thread(others[1]), isNot(same(second)), reason: 'the least recently used body was dropped');
    expect(mail.requests.length, requests + 1);
    expect(mail.requests.last.id, others[1].id);
  });

  testWidgets('a reader still drawing a conversation and a body still loading keep their entries', (tester) async {
    await start(rows: many());
    final b = state.conversations.firstWhere((c) => c.id == 'b');
    final shown = await prefetched(tester, b);
    // A reader fading out after its conversation closed still listens to its entry.
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: Reader(state: state, conversation: b),
    ));
    final loadingConversation = state.conversations.firstWhere((c) => c.id == 'c0');
    final loading = state.prefetch(loadingConversation);
    final loadingIndex = mail.requests.length - 1;
    final others = state.conversations.where((c) => !{'a', 'b', 'c0'}.contains(c.id)).toList();
    for (final c in others.take(MailState.keptThreads + 4)) {
      await prefetched(tester, c);
    }
    final requests = mail.requests.length;
    expect(state.thread(b), same(shown));
    expect(state.thread(loadingConversation), same(loading));
    expect(mail.requests.length, requests);
    mail.answer(loadingIndex, mail.rows.firstWhere((r) => r.id == 'c0'));
    await tester.pump();
    expect(loading.messages, isNotNull, reason: 'a body loading while others were dropped still arrives');

    // Once no reader draws it, it ages out like any other.
    await tester.pumpWidget(const SizedBox());
    for (final c in others.skip(MailState.keptThreads + 4).take(MailState.keptThreads)) {
      await prefetched(tester, c);
    }
    expect(state.thread(b), isNot(same(shown)));
    expect(mail.requests.last.id, 'b');
  });

  testWidgets('reopening a dropped conversation loads its bodies again and shows them', (tester) async {
    await start(rows: many());
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: ListenableBuilder(
        listenable: state,
        builder: (context, _) => switch (state.opened) {
          final opened? => Reader(key: ValueKey(opened.id), state: state, conversation: opened),
          null => const SizedBox(),
        },
      ),
    ));
    await openA(tester);
    await tester.pump();
    expect(find.textContaining('First message: keep'), findsOneWidget);
    final dropped = state.thread(state.opened!);
    final a = state.opened!;
    state.close();
    await tester.pump();

    for (final c in state.conversations.where((c) => c.id != 'a').take(MailState.keptThreads)) {
      await prefetched(tester, c);
    }
    state.open(a);
    expect(state.thread(a), isNot(same(dropped)));
    expect(mail.requests.last.id, 'a');
    await tester.pump();
    mail.answer(mail.requests.length - 1, mail.rows.first);
    await tester.pump();
    await tester.pump();
    expect(find.textContaining('First message: keep'), findsOneWidget);
    expect(state.thread(a).messages!.single.id, 'a-1');
    await tester.pumpWidget(const SizedBox());
  });
}
