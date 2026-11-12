import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/invitation.dart';

import 'reader_fixture.dart';

/// Synthetic invitations: the engine's answer for the card, and the answers it was asked to send.
class InvitationMail extends ReaderMail {
  String response = 'needsAction';
  bool editable = true;
  final sent = <(String, String?, String, String, bool)>[];

  @override
  Future<engine.InvitationStatus> calendarInvitation({required String id, required String message}) async => engine.InvitationStatus(
    account: 'reader@example.test',
    invitation: null,
    granted: true,
    editable: editable,
    event: engine.CalendarItem(
      account: 'reader@example.test',
      calendar: 'reader@example.test',
      calendarName: 'Reader',
      color: '#4285f4',
      id: 'ev1',
      title: 'Design review',
      start: '2026-11-10T15:00:00Z',
      end: '2026-11-10T16:00:00Z',
      allDay: false,
      startDay: null,
      endDay: null,
      location: 'Room 4',
      meetingUrl: null,
      organizer: engine.CalendarPerson(name: 'Priya Raman', email: 'priya@example.test', response: 'accepted'),
      attendees: [],
      response: response,
      notes: null,
      notesCut: false,
      link: null,
      series: null,
      mine: false,
      editable: false,
    ),
    calendar: 'reader@example.test',
    target: 'ev1',
    problem: null,
  );

  @override
  Future<void> calendarRespond({
    required String account,
    required String? calendar,
    required String event,
    required String response,
    required bool confirm,
  }) async {
    sent.add((account, calendar, event, response, confirm));
    this.response = response;
  }
}

engine.MeetingInvitation invitation({String method = 'request'}) => engine.MeetingInvitation(
  method: method,
  uid: 'design@example.test',
  title: 'Design review',
  start: '2026-11-10T15:00:00Z',
  end: '2026-11-10T16:00:00Z',
  allDay: false,
  startDay: null,
  endDay: null,
  location: 'Room 4',
  organizer: engine.CalendarPerson(name: 'Priya Raman', email: 'priya@example.test', response: null),
  recurring: false,
  occurrence: false,
);

void main() {
  late InvitationMail mail;
  late MailState state;

  setUp(() async {
    mail = InvitationMail();
    state = MailState(mail);
    await state.start();
  });

  tearDown(() => state.dispose());

  Future<void> show(WidgetTester tester, engine.MeetingInvitation invitation) async {
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: Align(
        alignment: Alignment.topLeft,
        child: SizedBox(
          width: 640,
          child: InvitationCard(state: state, conversation: 'a', message: 'a-1', invitation: invitation),
        ),
      ),
    ));
    await tester.pump();
    await tester.pump();
  }

  testWidgets('answers through the calendar and shows the answer it reads back', (tester) async {
    await show(tester, invitation());
    expect(find.text('Design review'), findsOneWidget);
    expect(find.text('Room 4'), findsOneWidget);
    expect(find.text('Organized by Priya Raman <priya@example.test>'), findsOneWidget);
    expect(find.text('Going?'), findsOneWidget);

    await tester.tap(find.text('Accept'));
    await tester.pump();
    await tester.pump();
    expect(mail.sent, [('reader@example.test', 'reader@example.test', 'ev1', 'accepted', true)]);
    expect(find.text('You accepted'), findsOneWidget);

    await tester.tap(find.text('Decline'));
    await tester.pump();
    await tester.pump();
    expect(mail.sent.last.$4, 'declined');
    expect(find.text('You declined'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a cancelled meeting and an account without calendar changes take no answer', (tester) async {
    await show(tester, invitation(method: 'cancel'));
    expect(find.text('This meeting was cancelled.'), findsOneWidget);
    expect(find.text('Accept'), findsNothing);

    mail.editable = false;
    await show(tester, invitation());
    await tester.pumpWidget(const SizedBox());
    await show(tester, invitation());
    expect(find.textContaining('Sign in to reader@example.test again'), findsOneWidget);
    expect(find.text('Accept'), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });
}
