import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/calendar_editor.dart';
import 'package:mailstream_ui/ui/design.dart';

import 'reader_fixture.dart';

const me = 'me@example.test';

engine.CalendarItem planning() => engine.CalendarItem(
  account: me,
  calendar: me,
  calendarName: 'Me',
  color: '#9fe1e7',
  id: 'plan',
  title: 'Planning',
  start: DateTime(2026, 11, 10, 9).toUtc().toIso8601String(),
  end: DateTime(2026, 11, 10, 10).toUtc().toIso8601String(),
  allDay: false,
  startDay: null,
  endDay: null,
  location: 'Room 1',
  meetingUrl: null,
  organizer: engine.CalendarPerson(name: null, email: me, response: null),
  attendees: [
    engine.CalendarPerson(name: null, email: me, response: 'accepted'),
    engine.CalendarPerson(name: 'Bo', email: 'bo@example.test', response: 'accepted'),
  ],
  response: 'organizer',
  notes: 'Agenda',
  notesCut: false,
  link: null,
  series: null,
  editable: true,
);

/// A synthetic calendar: one writable account with one event, and what the editor sent.
class EditorMail extends ReaderMail {
  final created = <(String, String?, engine.CalendarDraft)>[];
  final updated = <(String, String, engine.CalendarDraft)>[];
  final deleted = <(String, bool)>[];

  @override
  Future<engine.CalendarAgenda> calendarEvents({required String? from, required int days}) async => engine.CalendarAgenda(
    accounts: [
      engine.CalendarAccount(
        id: me,
        label: 'Personal',
        provider: 'google',
        granted: true,
        writable: true,
        loading: false,
        problem: null,
        fetched: null,
        calendars: [engine.CalendarInfo(id: me, name: 'Me', color: '#9fe1e7', primary: true, writable: true)],
      ),
    ],
    loading: false,
    events: [planning()],
    timeZone: 'Europe/Berlin',
    synced: engine.CalendarDays(from: '2026-11-01', until: '2027-01-08'),
  );

  @override
  Future<engine.CalendarItem> calendarCreate({required String account, required String? calendar, required engine.CalendarDraft draft}) async {
    created.add((account, calendar, draft));
    return planning();
  }

  @override
  Future<engine.CalendarItem> calendarUpdate({
    required String account,
    required String calendar,
    required String id,
    required engine.CalendarDraft draft,
  }) async {
    updated.add((id, calendar, draft));
    return planning();
  }

  @override
  Future<void> calendarDelete({required String account, required String calendar, required String id, required bool confirm}) async {
    deleted.add((id, confirm));
  }
}

void main() {
  late EditorMail mail;
  late MailState state;

  setUp(() async {
    mail = EditorMail();
    state = MailState(mail);
    await state.start();
    await state.calendar.reload();
  });

  tearDown(() => state.dispose());

  /// A window with one button that runs `open`.
  Future<void> window(WidgetTester tester, void Function(BuildContext context) open) async {
    await tester.binding.setSurfaceSize(const Size(900, 900));
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: Builder(
        builder: (context) => Center(child: PushButton(controlSize: ControlSize.large, onPressed: () => open(context), child: const Text('Open'))),
      ),
    ));
    await tester.tap(find.text('Open'));
    await tester.pumpAndSettle();
  }

  testWidgets('a new event is sent whole, an hour long, on the account\'s own calendar', (tester) async {
    await window(tester, (context) => showEventEditor(context, state, start: DateTime(2026, 11, 10, 14)));
    expect(find.text('New event'), findsOneWidget);
    expect(find.textContaining('Times in Europe/Berlin.'), findsOneWidget);
    await tester.enterText(find.byType(MacosTextField).first, 'Lunch');
    await tester.pump();
    await tester.tap(find.text('Add event'));
    await tester.pumpAndSettle();
    expect(mail.created.length, 1);
    final (account, calendar, draft) = mail.created.single;
    expect((account, calendar), (me, me));
    expect((draft.title, draft.start, draft.end, draft.allDay), ('Lunch', '2026-11-10T14:00', '2026-11-10T15:00', false));
    expect((draft.location, draft.notes, draft.attendees), (null, null, null));
    expect(find.text('New event'), findsNothing);
  });

  testWidgets('an edit sends only what changed, and guests stay unless removed', (tester) async {
    await window(tester, (context) => showEventEditor(context, state, event: planning()));
    expect(find.text('Edit event'), findsOneWidget);
    expect(find.text('bo@example.test'), findsOneWidget, reason: 'the guests, without the account itself');
    expect(find.text(me), findsNothing);
    await tester.enterText(find.byType(MacosTextField).first, 'Planning v2');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    final (id, calendar, draft) = mail.updated.single;
    expect((id, calendar), ('plan', me));
    expect(draft.title, 'Planning v2');
    expect((draft.start, draft.end, draft.allDay, draft.location, draft.notes, draft.attendees), (null, null, false, null, null, null));
  });

  testWidgets('deleting asks first, saying who hears of it', (tester) async {
    await window(tester, (context) => confirmDeleteEvent(context, state, planning()));
    expect(find.text('Delete “Planning”?'), findsOneWidget);
    expect(find.textContaining('Its guest is told it is cancelled.'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(mail.deleted, isEmpty);

    await tester.tap(find.text('Open'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete'));
    await tester.pumpAndSettle();
    expect(mail.deleted, [('plan', true)]);
  });
}
