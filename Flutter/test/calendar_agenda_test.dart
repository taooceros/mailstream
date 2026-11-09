import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/ui/calendar.dart';

engine.CalendarItem item(
  String id, {
  DateTime? start,
  DateTime? end,
  String? startDay,
  String? endDay,
  String? response,
  String? meetingUrl,
  engine.CalendarPerson? organizer,
  List<engine.CalendarPerson> attendees = const [],
}) => engine.CalendarItem(
  account: 'me@example.test',
  calendar: 'primary',
  calendarName: 'Me',
  color: '#9fe1e7',
  id: id,
  title: id,
  start: (start ?? DateTime.parse(startDay!)).toUtc().toIso8601String(),
  end: (end ?? DateTime.parse(endDay!)).toUtc().toIso8601String(),
  allDay: startDay != null,
  startDay: startDay,
  endDay: endDay,
  location: null,
  meetingUrl: meetingUrl,
  organizer: organizer,
  attendees: attendees,
  response: response,
  notes: null,
  notesCut: false,
  link: null,
  series: null,
  editable: false,
);

engine.CalendarPerson person(String name, String? response) =>
    engine.CalendarPerson(name: name, email: '${name.toLowerCase()}@example.test', response: response);

engine.CalendarAccount account({bool granted = true, bool writable = true}) => engine.CalendarAccount(
  id: 'me@example.test',
  label: 'Me',
  provider: 'google',
  granted: granted,
  writable: writable,
  loading: false,
  problem: null,
  fetched: null,
  calendars: const [],
);

void main() {
  final today = DateTime(2026, 11, 8);

  test('the agenda lists days from today, all-day events first, spans under each of their days', () {
    final days = agendaDays([
      item('Yesterday', start: DateTime(2026, 11, 7, 9), end: DateTime(2026, 11, 7, 10)),
      item('Standup', start: DateTime(2026, 11, 9, 10), end: DateTime(2026, 11, 9, 10, 30)),
      item('Trip', startDay: '2026-11-09', endDay: '2026-11-11'),
      item('Overnight', start: DateTime(2026, 11, 7, 22), end: DateTime(2026, 11, 8, 2)),
      item('Reminder', start: DateTime(2026, 11, 12, 9), end: DateTime(2026, 11, 12, 9)),
    ], today);
    expect(days.map((d) => d.day), [DateTime(2026, 11, 8), DateTime(2026, 11, 9), DateTime(2026, 11, 10), DateTime(2026, 11, 12)]);
    expect(days[0].events.map((e) => e.title), ['Overnight']);
    expect(days[1].events.map((e) => e.title), ['Trip', 'Standup']);
    expect(days[2].events.map((e) => e.title), ['Trip']);
    expect(days[3].events.map((e) => e.title), ['Reminder']);
  });

  test('days are titled Today and Tomorrow, then by weekday', () {
    expect(agendaTitle(today, today), 'Today · Sunday, November 8');
    expect(agendaTitle(DateTime(2026, 11, 9), today), 'Tomorrow · Monday, November 9');
    expect(agendaTitle(DateTime(2027, 1, 4), today), 'Monday, January 4, 2027');
    expect(hexColor('#ff0000'), const Color(0xFFFF0000));
    expect(hexColor('red'), isNull);
  });

  test('Up Next offers a meeting from 30 minutes before it starts until it ends, the soonest first', () {
    final now = DateTime(2026, 11, 9, 10, 0);
    final call = 'https://meet.example.test/x';
    final soon = item('Soon', start: DateTime(2026, 11, 9, 10, 7), end: DateTime(2026, 11, 9, 10, 30), meetingUrl: call);
    final later = item('Later', start: DateTime(2026, 11, 9, 10, 20), end: DateTime(2026, 11, 9, 11), meetingUrl: call);
    final tooFar = item('Too far', start: DateTime(2026, 11, 9, 10, 31), end: DateTime(2026, 11, 9, 11), meetingUrl: call);
    final running = item('Running', start: DateTime(2026, 11, 9, 9, 30), end: DateTime(2026, 11, 9, 10, 30), meetingUrl: call);
    final over = item('Over', start: DateTime(2026, 11, 9, 9), end: DateTime(2026, 11, 9, 10), meetingUrl: call);
    final declined = item('Declined', start: DateTime(2026, 11, 9, 10, 5), end: DateTime(2026, 11, 9, 11), meetingUrl: call, response: 'declined');
    final alone = item('Focus', start: DateTime(2026, 11, 9, 10, 1), end: DateTime(2026, 11, 9, 12));
    final allDay = item('Trip', startDay: '2026-11-09', endDay: '2026-11-10');

    expect(upNext([later, soon, tooFar, running, over, declined, alone, allDay], now)?.title, 'Soon');
    expect(upNextLabel(soon, now), 'In 7m');
    expect(upNextLabel(soon, now.add(const Duration(seconds: 30))), 'In 7m', reason: 'a started minute counts whole');
    // Within the lead exactly; one minute past it is too early.
    expect(upNext([tooFar], DateTime(2026, 11, 9, 10, 1))?.title, 'Too far');
    expect(upNext([tooFar], now), isNull);
    // Nothing about to start: the meeting under way, the latest begun.
    expect(upNext([running, over, tooFar], now)?.title, 'Running');
    expect(upNextLabel(running, now), 'Now');
    expect(upNext([running], DateTime(2026, 11, 9, 10, 30)), isNull, reason: 'gone once it ends');
    // Guests make a meeting too, without a call.
    expect(upNext([item('Sync', start: DateTime(2026, 11, 9, 10, 10), end: DateTime(2026, 11, 9, 11), attendees: [person('Ann', 'accepted')])], now)?.title, 'Sync');
    expect(upNext([declined, alone, allDay, over], now), isNull);
  });

  test('replies: guests answer when their sign-in may change the calendar; the answer being sent shows chosen', () {
    final at = DateTime(2026, 11, 9, 14);
    final end = DateTime(2026, 11, 9, 15);
    final invited = item('Interview', start: at, end: end, response: 'needsAction');
    expect(replyState(invited, account(), null), (answerable: true, chosen: null, sending: null, invited: true));
    expect(replyState(invited, account(), 'accepted'), (answerable: true, chosen: 'accepted', sending: 'accepted', invited: true));
    expect(replyState(item('Sync', start: at, end: end, response: 'tentative'), account(), null),
        (answerable: true, chosen: 'tentative', sending: null, invited: false));
    expect(replyState(item('Old', start: at, end: end, response: 'declined'), account(), null).chosen, 'declined');
    // The organizer, events without guests, and accounts that cannot change their calendar answer nothing.
    expect(replyState(item('Mine', start: at, end: end, response: 'organizer'), account(), null),
        (answerable: false, chosen: null, sending: null, invited: false));
    expect(replyState(item('Gym', start: at, end: end), account(), null).answerable, isFalse);
    expect(replyState(invited, account(writable: false), null), (answerable: false, chosen: null, sending: null, invited: false));
    expect(replyState(invited, account(granted: false), null).invited, isFalse);
    expect(replyState(invited, null, null).answerable, isFalse);
    expect(replies.map((r) => r.$1), ['accepted', 'tentative', 'declined']);
  });

  test('guests: the organizer first when the guests leave them out, answers counted', () {
    final guests = [person('Ann', 'accepted'), person('Bob', 'tentative'), person('Cy', 'needsAction'), person('Di', null), person('Ed', 'declined')];
    final event = item('Review', start: DateTime(2026, 11, 9, 10), end: DateTime(2026, 11, 9, 11), organizer: person('Org', null), attendees: guests);
    expect(eventGuests(event).map((g) => (g.person.name, g.organizer)), [('Org', true), ('Ann', false), ('Bob', false), ('Cy', false), ('Di', false), ('Ed', false)]);
    final listed = item('Review', start: DateTime(2026, 11, 9, 10), end: DateTime(2026, 11, 9, 11), organizer: person('Ann', null), attendees: guests);
    expect(eventGuests(listed).where((g) => g.organizer).map((g) => g.person.name), ['Ann']);
    expect(eventGuests(listed).length, 5);
    expect(guestSummary(guests), '1 Going · 1 Maybe · 1 Declined · 2 Invited');
    expect(guestSummary(const []), '');
  });

  test('a day is summed up by its events and the hours they book, overlaps once, declined ones not', () {
    final day = DateTime(2026, 11, 9);
    final events = [
      item('A', start: DateTime(2026, 11, 9, 9), end: DateTime(2026, 11, 9, 10)),
      item('B', start: DateTime(2026, 11, 9, 9, 30), end: DateTime(2026, 11, 9, 10, 45)),
      item('C', start: DateTime(2026, 11, 9, 13), end: DateTime(2026, 11, 9, 13, 30)),
      item('Declined', start: DateTime(2026, 11, 9, 15), end: DateTime(2026, 11, 9, 17), response: 'declined'),
      item('Overnight', start: DateTime(2026, 11, 9, 23), end: DateTime(2026, 11, 10, 2)),
      item('Trip', startDay: '2026-11-09', endDay: '2026-11-10'),
    ];
    expect(daySummary(events, day), '6 events · 3h 15m');
    expect(daySummary([events.last], day), '1 event');
  });
}
