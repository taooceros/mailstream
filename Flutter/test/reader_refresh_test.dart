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

  Future<void> start() async {
    mail = ReaderMail();
    state = MailState(mail);
    await state.start();
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
}
