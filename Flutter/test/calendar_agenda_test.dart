import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/ui/calendar.dart';

engine.CalendarItem item(String id, {DateTime? start, DateTime? end, String? startDay, String? endDay}) => engine.CalendarItem(
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
  meetingUrl: null,
  organizer: null,
  attendees: const [],
  response: null,
  notes: null,
  link: null,
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
}
