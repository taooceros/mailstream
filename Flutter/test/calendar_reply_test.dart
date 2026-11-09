import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/calendar.dart';
import 'package:mailstream_ui/ui/design.dart';

import 'calendar_fixture.dart';

void main() {
  late CalendarMail mail;
  late MailState state;

  setUp(() async {
    mail = CalendarMail();
    state = MailState(mail);
    await state.start();
    await state.calendar.reload();
  });

  tearDown(() => state.dispose());

  /// `build` in a window, built again as the calendar changes, as the shell does.
  Future<void> show(WidgetTester tester, Widget Function() build, {Brightness brightness = Brightness.light}) async {
    await tester.binding.setSurfaceSize(const Size(560, 1000));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(brightness),
      home: ListenableBuilder(listenable: state.calendar, builder: (context, _) => build()),
    ));
    await tester.pump();
  }

  /// The answer sent, the calendar read again (after its change event's debounce) and drawn.
  Future<void> settle(WidgetTester tester) async {
    for (var i = 0; i < 4; i++) {
      await tester.pump(const Duration(milliseconds: 200));
    }
  }

  Finder row(String id) {
    final event = state.calendar.events.firstWhere((e) => e.id == id);
    final day = DateTime.parse(event.start).toLocal();
    return find.byKey(ValueKey('${event.account}|${event.calendar}|${event.id}|${DateTime(day.year, day.month, day.day).millisecondsSinceEpoch}'));
  }

  testWidgets('the agenda offers the meeting about to start, and answers an invitation from its row', (tester) async {
    await show(tester, () => CalendarListPane(state: state));
    // The Up Next bar, with Join, over the agenda's own row for it.
    expect(find.textContaining(RegExp(r'^In [67]m$')), findsOneWidget);
    expect(find.text('Architecture Review'), findsNWidgets(2));
    expect(find.text('Join'), findsOneWidget);
    // Only the unanswered invitations offer the answers.
    final vendor = row('vendor');
    expect(find.descendant(of: vendor, matching: find.text('Going')), findsOneWidget);
    expect(find.descendant(of: row('review'), matching: find.text('Going')), findsNothing);
    expect(find.descendant(of: row('handoff'), matching: find.text('Going')), findsNothing, reason: 'answered Maybe already');

    await tester.tap(find.descendant(of: vendor, matching: find.text('Going')));
    await settle(tester);
    expect(mail.answers, [(team, 'team', 'vendor', 'accepted', true)]);
    expect(state.calendar.events.firstWhere((e) => e.id == 'vendor').response, 'accepted');
    expect(find.descendant(of: row('vendor'), matching: find.text('Going')), findsNothing, reason: 'answered: its guests show instead');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('the event pane answers inline, beside Join, and lists the guests two to a row', (tester) async {
    const review = (account: me, calendar: 'work', id: 'review');
    await show(
      tester,
      () => CalendarEventPane(state: state, event: state.calendar.event(review)!, twoPane: true, onBack: () {}),
      brightness: Brightness.dark,
    );
    expect(find.text('6 guests'), findsOneWidget);
    expect(find.text('4 Going · 1 Maybe · 1 Invited'), findsOneWidget);
    expect(find.text('Organizer · Going'), findsOneWidget);
    expect(find.text('Join'), findsOneWidget);
    // Two cards side by side: Alex's and Maya's start at the same height.
    expect(tester.getTopLeft(find.text('Alex Rivera')).dy, tester.getTopLeft(find.text('Maya Chen')).dy);

    // The answers come first; Sofia's card says Maybe too.
    await tester.tap(find.text('Maybe').first);
    await settle(tester);
    expect(mail.answers, [(me, 'work', 'review', 'tentative', true)]);
    expect(state.calendar.event(review)!.response, 'tentative');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('the organizer is told so instead of answers', (tester) async {
    final polish = state.calendar.event((account: me, calendar: 'personal', id: 'polish'))!;
    await show(tester, () => CalendarEventPane(state: state, event: polish, twoPane: true, onBack: () {}));
    expect(find.text('You organize this'), findsOneWidget);
    expect(find.text('Decline'), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });
}
