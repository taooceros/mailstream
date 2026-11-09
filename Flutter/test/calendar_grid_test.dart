import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/ui/calendar_grid.dart';
import 'package:mailstream_ui/ui/design.dart';

import 'calendar_agenda_test.dart' show item;

void main() {
  final day = DateTime(2026, 11, 10);

  test('overlapping events share the width; each widens over the columns that stay free', () {
    final placed = layoutDay([
      item('Standup', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 10)),
      item('Long', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 12)),
      item('Review', start: DateTime(2026, 11, 10, 9, 30), end: DateTime(2026, 11, 10, 10, 30)),
      item('Lunch', start: DateTime(2026, 11, 10, 10, 30), end: DateTime(2026, 11, 10, 11)),
      item('Alone', start: DateTime(2026, 11, 10, 14), end: DateTime(2026, 11, 10, 15)),
      item('Ping', start: DateTime(2026, 11, 10, 16), end: DateTime(2026, 11, 10, 16, 5)),
      item('Other day', start: DateTime(2026, 11, 11, 9), end: DateTime(2026, 11, 11, 10)),
    ], day);
    final byTitle = {for (final p in placed) p.event.title: p};
    expect(byTitle.keys, ['Long', 'Standup', 'Review', 'Lunch', 'Alone', 'Ping']);
    // The longer of two starting together goes first.
    expect((byTitle['Long']!.column, byTitle['Long']!.columns, byTitle['Long']!.span), (0, 3, 1));
    expect((byTitle['Standup']!.column, byTitle['Standup']!.span), (1, 1));
    expect((byTitle['Review']!.column, byTitle['Review']!.span), (2, 1));
    // Lunch takes Standup's column, freed at 10, and widens over Review's once that ends.
    expect((byTitle['Lunch']!.column, byTitle['Lunch']!.span), (1, 2));
    expect((byTitle['Alone']!.column, byTitle['Alone']!.columns, byTitle['Alone']!.span), (0, 1, 1));
    expect((byTitle['Alone']!.top, byTitle['Alone']!.bottom), (14 * 60, 15 * 60));
    // A five-minute event is drawn 20 minutes tall.
    expect((byTitle['Ping']!.top, byTitle['Ping']!.bottom), (16 * 60, 16 * 60 + minEventMinutes));
  });

  test('a free column to the right widens an event', () {
    final placed = layoutDay([
      item('A', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 11)),
      item('B', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 10)),
      item('C', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 9, 30)),
      item('D', start: DateTime(2026, 11, 10, 10), end: DateTime(2026, 11, 10, 10, 30)),
    ], day);
    final d = placed.firstWhere((p) => p.event.title == 'D');
    expect((d.column, d.columns, d.span), (1, 3, 2));
  });

  test('an event over midnight is drawn on both days', () {
    final overnight = item('Flight', start: DateTime(2026, 11, 10, 22), end: DateTime(2026, 11, 11, 2));
    final first = layoutDay([overnight], day).single, second = layoutDay([overnight], DateTime(2026, 11, 11)).single;
    expect((first.top, first.bottom), (22 * 60, 24 * 60));
    expect((second.top, second.bottom), (0, 2 * 60));
  });

  test('all-day and day-long events go in the all-day row, earlier and longer ones on top', () {
    final week = weekDays(DateTime(2026, 11, 8));
    final row = layoutAllDay([
      item('Holiday', startDay: '2026-11-11', endDay: '2026-11-12'),
      item('Trip', startDay: '2026-11-10', endDay: '2026-11-13'),
      item('Conference', start: DateTime(2026, 11, 12, 9), end: DateTime(2026, 11, 14, 17)),
      item('Earlier', startDay: '2026-11-01', endDay: '2026-11-09'),
      item('Later', startDay: '2026-11-15', endDay: '2026-11-16'),
      item('Meeting', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 10)),
    ], week);
    final byTitle = {for (final p in row.placements) p.event.title: p};
    expect(byTitle.keys, ['Earlier', 'Trip', 'Holiday', 'Conference']);
    expect((byTitle['Earlier']!.first, byTitle['Earlier']!.last, byTitle['Earlier']!.row), (0, 0, 0));
    expect((byTitle['Trip']!.first, byTitle['Trip']!.last, byTitle['Trip']!.row), (2, 4, 0));
    expect((byTitle['Holiday']!.first, byTitle['Holiday']!.last, byTitle['Holiday']!.row), (3, 3, 1));
    expect((byTitle['Conference']!.first, byTitle['Conference']!.last, byTitle['Conference']!.row), (4, 6, 1));
    expect(row.rows, 2);
    expect(layoutDay([item('Conference', start: DateTime(2026, 11, 12, 9), end: DateTime(2026, 11, 14, 17))], DateTime(2026, 11, 12)), isEmpty);
  });

  test('weeks start on Sunday in the US and are titled by their days', () {
    expect(weekOf(DateTime(2026, 11, 11)), DateTime(2026, 11, 8));
    expect(weekOf(DateTime(2026, 11, 8)), DateTime(2026, 11, 8));
    expect(weekOf(DateTime(2026, 11, 14)), DateTime(2026, 11, 8));
    expect(weekTitle(DateTime(2026, 11, 8)), 'November 8 – 14, 2026');
    expect(weekTitle(DateTime(2026, 11, 29)), 'November 29 – December 5, 2026');
    expect(weekTitle(DateTime(2026, 12, 27)), 'December 27, 2026 – January 2, 2027');
    expect(dayTitle(DateTime(2026, 11, 8)), 'Sunday, November 8, 2026');
    expect(syncedNote((from: DateTime(2026, 11, 1), until: DateTime(2027, 1, 8))), 'Only Nov 1 – Jan 7 is synced');
  });

  testWidgets('the week grid places overlapping events side by side and all-day ones over the hours', (tester) async {
    tester.view.physicalSize = const Size(1000, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final week = weekDays(DateTime(2026, 11, 8));
    final opened = <String>[];
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: CalendarGrid(
        days: week,
        events: [
          item('Standup', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 10)),
          item('Review', start: DateTime(2026, 11, 10, 9, 30), end: DateTime(2026, 11, 10, 10, 30)),
          item('Trip', startDay: '2026-11-11', endDay: '2026-11-13'),
        ],
        colorOf: (_) => const Color(0xFF9FE1E7),
        synced: (from: DateTime(2026, 11, 9), until: DateTime(2027, 1, 8)),
        onOpen: (event) => opened.add(event.title),
      ),
    ));
    await tester.pumpAndSettle();

    const column = (1000 - CalendarGrid.gutter) / 7;
    final standup = tester.getRect(find.text('Standup').first);
    final review = tester.getRect(find.text('Review').first);
    // Tuesday is the third column; Standup in its left half, Review in its right half, lower.
    expect(standup.left, inInclusiveRange(CalendarGrid.gutter + 2 * column, CalendarGrid.gutter + 2.5 * column));
    expect(review.left, inInclusiveRange(CalendarGrid.gutter + 2.5 * column, CalendarGrid.gutter + 3 * column));
    expect(review.top - standup.top, closeTo(CalendarGrid.hourHeight / 2, 2));

    // The trip spans Wednesday and Thursday in the all-day row, above the timeline.
    final trip = tester.getRect(find.text('Trip'));
    expect(trip.left, inInclusiveRange(CalendarGrid.gutter + 3 * column, CalendarGrid.gutter + 3 * column + 12));
    expect(find.text('all-day'), findsOneWidget);

    // Sunday Nov 8 is before the synced days.
    expect(find.text('Only Nov 9 – Jan 7 is synced'), findsOneWidget);

    await tester.tap(find.text('Trip'));
    expect(opened, ['Trip']);
    await tester.pumpWidget(const SizedBox());
  });
}
