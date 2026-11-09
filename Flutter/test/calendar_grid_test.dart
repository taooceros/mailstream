import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter/rendering.dart' show RenderParagraph;
import 'package:flutter_test/flutter_test.dart';
import 'package:intl/intl.dart' hide TextDirection;
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/state/calendar_state.dart';
import 'package:mailstream_ui/ui/calendar.dart';
import 'package:mailstream_ui/ui/calendar_grid.dart';
import 'package:mailstream_ui/ui/design.dart';

import 'calendar_agenda_test.dart' show item;

/// A week grid that pages as the calendar does: [onPage] moves its days by a week.
class _PagedWeek extends StatefulWidget {
  const _PagedWeek({required this.start, required this.events, required this.pages});

  final DateTime start;
  final List<engine.CalendarItem> events;
  final List<int> pages;

  @override
  State<_PagedWeek> createState() => _PagedWeekState();
}

class _PagedWeekState extends State<_PagedWeek> {
  late DateTime start = widget.start;

  void page(int step) => setState(() {
    widget.pages.add(step);
    start = DateTime(start.year, start.month, start.day + 7 * step);
  });

  @override
  Widget build(BuildContext context) =>
      CalendarGrid(days: weekDays(start), events: widget.events, colorOf: (_) => const Color(0xFF4285F4), onPage: page);
}

/// A meeting the account was invited to, answered `response`, with `guests` others.
engine.CalendarItem meeting(String title, DateTime start, int minutes, String? response, {int guests = 0, bool call = false}) =>
    engine.CalendarItem(
      account: 'me@example.test',
      calendar: 'primary',
      calendarName: 'Me',
      color: '#4285f4',
      id: title,
      title: title,
      start: start.toUtc().toIso8601String(),
      end: start.add(Duration(minutes: minutes)).toUtc().toIso8601String(),
      allDay: false,
      startDay: null,
      endDay: null,
      location: null,
      meetingUrl: call ? 'https://meet.google.com/abc-defg-hij' : null,
      organizer: null,
      attendees: [
        engine.CalendarPerson(name: 'Me', email: 'ME@example.test', response: response),
        for (var i = 1; i <= guests; i++) engine.CalendarPerson(name: 'Person $i', email: 'person$i@example.test', response: 'accepted'),
      ],
      response: response,
      notes: null,
      notesCut: false,
      link: null,
      series: null,
      editable: false,
    );

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

  test('a card reads declined, then over, then waiting for an answer, else ahead', () {
    final now = DateTime(2026, 11, 10, 12);
    final later = DateTime(2026, 11, 10, 15), earlier = DateTime(2026, 11, 10, 9);
    expect(eventLook(meeting('Dentist', later, 60, 'declined'), now), EventLook.declined);
    expect(eventLook(meeting('Old no', earlier, 60, 'declined'), now), EventLook.declined);
    expect(eventLook(meeting('Done', earlier, 60, 'accepted'), now), EventLook.past);
    expect(eventLook(meeting('Missed invite', earlier, 60, 'needsAction'), now), EventLook.past);
    // Ending at noon is over at noon.
    expect(eventLook(meeting('Morning', DateTime(2026, 11, 10, 11), 60, 'accepted'), now), EventLook.past);
    expect(eventLook(meeting('Invite', later, 60, 'needsAction'), now), EventLook.invited);
    expect(eventLook(meeting('Maybe', later, 60, 'tentative'), now), EventLook.invited);
    expect(eventLook(meeting('Going', later, 60, 'accepted'), now), EventLook.upcoming);
    expect(eventLook(meeting('Mine', later, 60, 'organizer'), now), EventLook.upcoming);
    expect(eventLook(meeting('Own', later, 60, null), now), EventLook.upcoming);
    // On now.
    expect(eventLook(meeting('Running', DateTime(2026, 11, 10, 11, 30), 60, 'accepted'), now), EventLook.upcoming);
  });

  test('only events the account is going to clash, and only where their times overlap', () {
    List<String> clashing(List<engine.CalendarItem> events) {
      final placed = layoutDay(events, day);
      final conflicts = dayConflicts(placed);
      return [for (var i = 0; i < placed.length; i++) if (conflicts[i]) placed[i].event.title]..sort();
    }

    final nine = DateTime(2026, 11, 10, 9);
    expect(clashing([meeting('Review', nine, 60, 'accepted'), meeting('Sync', nine.add(const Duration(minutes: 30)), 60, 'organizer')]),
        ['Review', 'Sync']);
    expect(clashing([meeting('Review', nine, 60, 'accepted'), meeting('Focus', nine, 30, null)]), ['Focus', 'Review']);
    expect(clashing([meeting('Review', nine, 60, 'accepted'), meeting('Maybe', nine, 60, 'tentative')]), isEmpty);
    expect(clashing([meeting('Review', nine, 60, 'accepted'), meeting('Invite', nine, 60, 'needsAction')]), isEmpty);
    expect(clashing([meeting('Review', nine, 60, 'accepted'), meeting('No', nine, 60, 'declined')]), isEmpty);
    // Back to back, and a five-minute ping drawn 20 minutes tall beside the next: no clash.
    expect(clashing([meeting('First', nine, 60, 'accepted'), meeting('Second', nine.add(const Duration(hours: 1)), 60, 'accepted')]), isEmpty);
    expect(clashing([meeting('Ping', nine, 5, 'accepted'), meeting('Next', nine.add(const Duration(minutes: 10)), 30, 'accepted')]), isEmpty);
  });

  test('booked hours count overlaps once and leave declined events out', () {
    final nine = DateTime(2026, 11, 10, 9);
    expect(bookedHours(layoutDay([], day)), 0);
    expect(
      bookedHours(layoutDay([
        meeting('A', nine, 60, 'accepted'),
        meeting('B', nine.add(const Duration(minutes: 30)), 60, 'accepted'),
        meeting('C', nine.add(const Duration(hours: 3)), 30, null),
        meeting('No', nine.add(const Duration(hours: 5)), 120, 'declined'),
      ], day)),
      2,
    );
  });

  test('a card fits one line under 45 minutes and its guests from 75', () {
    expect(cardFit(minEventMinutes), CardFit.line);
    expect(cardFit(30), CardFit.line);
    expect(cardFit(44), CardFit.line);
    expect(cardFit(45), CardFit.standard);
    expect(cardFit(74), CardFit.standard);
    expect(cardFit(75), CardFit.full);
  });

  test('card text: time ranges, lengths, call services and the gutter time', () {
    final pm = DateFormat.jm().format(DateTime(2026, 11, 10, 13));
    expect(timeRange(DateTime(2026, 11, 10, 10, 15), DateTime(2026, 11, 10, 11, 30)), '10:15 – ${DateFormat.jm().format(DateTime(2026, 11, 10, 11, 30))}');
    expect(timeRange(DateTime(2026, 11, 10, 11), DateTime(2026, 11, 10, 13)), '${DateFormat.jm().format(DateTime(2026, 11, 10, 11))} – $pm');
    expect(shortDuration(const Duration(minutes: 15)), '15m');
    expect(shortDuration(const Duration(hours: 1)), '1h');
    expect(shortDuration(const Duration(minutes: 90)), '1h 30m');
    expect(meetingLabel('https://meet.google.com/abc'), 'Meet');
    expect(meetingLabel('https://us02web.zoom.us/j/1'), 'Zoom');
    expect(meetingLabel('https://teams.microsoft.com/l/x'), 'Teams');
    expect(gutterTime(DateTime(2026, 11, 10, 22, 8)), '10:08');
  });

  testWidgets('the day view marks clashes, invitations, declines, guests, calls and short meetings', (tester) async {
    tester.view.physicalSize = const Size(1000, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    // Years ahead, so nothing is over yet.
    final ahead = DateTime(2031, 3, 4);
    DateTime at(int hour, [int minute = 0]) => DateTime(ahead.year, ahead.month, ahead.day, hour, minute);
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: CalendarGrid(
        days: [ahead],
        events: [
          meeting('Standup', at(9, 15), 15, 'accepted', call: true),
          meeting('Review', at(10), 90, 'accepted', guests: 4, call: true),
          meeting('Sync', at(10, 30), 60, 'organizer'),
          meeting('Interview', at(13), 60, 'needsAction'),
          meeting('Dentist', at(15), 60, 'declined'),
        ],
        colorOf: (_) => const Color(0xFF4285F4),
      ),
    ));
    await tester.pumpAndSettle();

    // Every card carries its calendar's dot; Review and Sync overlap and both are on.
    expect(find.byKey(const ValueKey('dot')), findsNWidgets(5));
    expect(find.byKey(const ValueKey('conflict')), findsNWidgets(2));
    expect(find.byKey(const ValueKey('invited')), findsOneWidget);
    final dentist = tester.widget<Text>(find.text('Dentist'));
    expect(dentist.style!.decoration, TextDecoration.lineThrough);
    // The 15-minute standup is one line: title, start and length.
    expect(find.textContaining('· 15m', findRichText: true), findsOneWidget);
    // Review is long enough for its call and guests: three of four, then +1 (the account left out).
    expect(find.text('Meet'), findsOneWidget);
    expect(find.byType(GuestMonogram), findsNWidgets(3));
    expect(find.text('+1'), findsOneWidget);
    expect(find.text('Going'), findsOneWidget);
    expect(find.text('Invited'), findsOneWidget);
    // A future day: no today card, a load bar.
    expect(find.byKey(const ValueKey('today')), findsNothing);
    expect(find.byKey(const ValueKey('load')), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets("only today's date is raised; the other days with events show a load bar", (tester) async {
    tester.view.physicalSize = const Size(1000, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final now = DateTime.now();
    final week = weekDays(weekOf(now));
    final other = week.firstWhere((d) => d.day != now.day);
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.dark),
      home: CalendarGrid(
        days: week,
        events: [
          meeting('Elsewhere', DateTime(other.year, other.month, other.day, 9), 60, 'accepted'),
          meeting('Today', DateTime(now.year, now.month, now.day, 9), 60, 'accepted'),
        ],
        colorOf: (_) => const Color(0xFF4285F4),
      ),
    ));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('today')), findsOneWidget);
    expect(find.byKey(const ValueKey('load')), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('cards in a narrow week drop what does not fit instead of overflowing', (tester) async {
    tester.view.physicalSize = const Size(560, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final week = weekDays(DateTime(2031, 3, 2));
    DateTime at(int day, int hour, [int minute = 0]) => DateTime(week[day].year, week[day].month, week[day].day, hour, minute);
    await tester.pumpWidget(MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: CalendarGrid(
        days: week,
        events: [
          // Three side by side: slivers a third of a 73pt day wide.
          meeting('Architecture review', at(1, 10), 90, 'accepted', guests: 4, call: true),
          meeting('Sync', at(1, 10, 30), 60, 'organizer', guests: 1, call: true),
          meeting('Vendor demo', at(1, 10, 45), 30, 'accepted', call: true),
          // Two side by side, and one alone with a call and guests.
          meeting('Offsite prep', at(2, 13), 90, 'organizer', guests: 3, call: true),
          meeting('Hiring sync', at(2, 13), 30, 'tentative'),
          meeting('Workshop', at(5, 13), 180, 'accepted', guests: 2, call: true),
        ],
        colorOf: (_) => const Color(0xFF4285F4),
      ),
    ));
    await tester.pumpAndSettle();
    // Every card has its dot; the overflow checks of the layout ran without a report.
    expect(find.byKey(const ValueKey('dot')), findsNWidgets(6));
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a title never breaks inside a word, whatever the column width (Install7: "Dinne / r with")', (tester) async {
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final week = weekDays(DateTime(2031, 3, 2));
    DateTime at(int day, int hour, [int minute = 0]) => DateTime(week[day].year, week[day].month, week[day].day, hour, minute);
    final events = [
      // Install7's card: two hours, no guests.
      meeting('Dinner with Person 6', at(2, 19), 120, null),
      meeting('Architecture review', at(1, 10), 90, 'accepted', guests: 4, call: true),
      meeting('Interview: frontend', at(1, 14), 60, 'needsAction'),
      meeting('Workshop', at(5, 13), 180, 'accepted', guests: 2, call: true),
      meeting('Offsite prep', at(3, 13), 60, 'organizer'),
      meeting('Budget review', at(3, 13, 30), 60, 'accepted'),
    ];
    final titles = {for (final event in events) event.title};
    final broken = <String>[];
    // Each day from 40 to 140 points wide, a point at a time, in the test font (13 points a
    // letter at the cards' title size, so `Dinner` needs 78).
    for (var day = 40; day <= 140; day++) {
      tester.view.physicalSize = Size(CalendarGrid.gutter + 7.0 * day, 900);
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(Brightness.light),
        home: CalendarGrid(days: week, events: events, colorOf: (_) => const Color(0xFF4285F4)),
      ));
      for (final paragraph in tester.renderObjectList<RenderParagraph>(find.byType(RichText))) {
        final text = paragraph.text.toPlainText();
        if (!titles.contains(text)) continue;
        // Two letters of one word on different lines: the word was broken. Letters an ellipsis
        // cut off have no box.
        for (var i = 1; i < text.length; i++) {
          if (text[i - 1] == ' ' || text[i] == ' ') continue;
          final first = paragraph.getBoxesForSelection(TextSelection(baseOffset: i - 1, extentOffset: i));
          final second = paragraph.getBoxesForSelection(TextSelection(baseOffset: i, extentOffset: i + 1));
          if (first.isEmpty || second.isEmpty) continue;
          if (second.first.top > first.first.top) {
            broken.add('$text at $i, ${paragraph.size.width} wide (day $day)');
            break;
          }
        }
      }
    }
    expect(broken, isEmpty);
    await tester.pumpWidget(const SizedBox());
  });

  group('paging', () {
    final start = DateTime(2031, 3, 2);
    final events = [
      meeting('This week', DateTime(2031, 3, 3, 10), 60, 'accepted'),
      meeting('Next week', DateTime(2031, 3, 10, 10), 60, 'accepted'),
    ];

    Future<List<int>> week(WidgetTester tester) async {
      tester.view.physicalSize = const Size(1000, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final pages = <int>[];
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(Brightness.light),
        home: _PagedWeek(start: start, events: events, pages: pages),
      ));
      await tester.pumpAndSettle();
      return pages;
    }

    testWidgets('a two-finger swipe drags the week 1:1, springs back when short and on to the next past a third', (tester) async {
      final pages = await week(tester);
      // The weeks either side are laid out but not drawn at rest.
      expect(find.text('This week'), findsOneWidget);
      expect(find.text('Next week'), findsNothing);
      final left = tester.getRect(find.text('This week')).left;

      final fingers = await tester.createGesture(kind: PointerDeviceKind.trackpad);
      await fingers.panZoomStart(const Offset(500, 400));
      await tester.pump();
      expect(find.text('Next week'), findsOneWidget);
      await fingers.panZoomUpdate(const Offset(500, 400), pan: const Offset(-100, 0));
      await tester.pump();
      expect(tester.getRect(find.text('This week')).left, closeTo(left - 100, 0.5));
      // The next week's Monday follows a page width to the right.
      expect(tester.getRect(find.text('Next week')).left, closeTo(left - 100 + (1000 - CalendarGrid.gutter), 0.5));
      await fingers.panZoomEnd();
      await tester.pumpAndSettle();
      expect(pages, isEmpty);
      expect(tester.getRect(find.text('This week')).left, closeTo(left, 0.5));
      expect(find.text('Next week'), findsNothing);

      final again = await tester.createGesture(kind: PointerDeviceKind.trackpad);
      await again.panZoomStart(const Offset(500, 400));
      await tester.pump();
      await again.panZoomUpdate(const Offset(500, 400), pan: const Offset(-400, 0));
      await tester.pump();
      await again.panZoomEnd();
      await tester.pump();
      expect(pages, [1]);
      await tester.pumpAndSettle();
      expect(tester.getRect(find.text('Next week')).left, closeTo(left, 0.5));
      expect(find.text('This week'), findsNothing);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('the next week (→) slides in over Motion.page, the last one sliding out beside it', (tester) async {
      final pages = await week(tester);
      final left = tester.getRect(find.text('This week')).left;
      tester.state<_PagedWeekState>(find.byType(_PagedWeek)).page(1);
      await tester.pump();
      await tester.pump(Motion.page ~/ 3);
      final leaving = tester.getRect(find.text('This week')).left, coming = tester.getRect(find.text('Next week')).left;
      expect(leaving, lessThan(left));
      expect(coming, greaterThan(left));
      expect(coming - leaving, closeTo(1000 - CalendarGrid.gutter, 0.5));
      await tester.pump(Motion.page);
      expect(tester.getRect(find.text('Next week')).left, closeTo(left, 0.5));
      await tester.pumpAndSettle();
      expect(find.text('This week'), findsNothing);
      expect(pages, [1]);
      await tester.pumpWidget(const SizedBox());
    });
  });

  testWidgets('Day and Week open at the hours the other was scrolled to', (tester) async {
    tester.view.physicalSize = const Size(1000, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final offset = TimelineOffset();
    Widget grid(List<DateTime> days) => MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: CalendarGrid(key: ValueKey(days.length), days: days, events: const [], colorOf: (_) => const Color(0xFF4285F4), offset: offset),
    );
    await tester.pumpWidget(grid(weekDays(DateTime(2031, 3, 2))));
    await tester.pumpAndSettle();
    final week = tester.widget<SingleChildScrollView>(find.byType(SingleChildScrollView).last).controller!;
    week.jumpTo(6.5 * CalendarGrid.hourHeight);
    await tester.pumpWidget(grid([DateTime(2031, 3, 4)]));
    await tester.pumpAndSettle();
    final day = tester.widget<SingleChildScrollView>(find.byType(SingleChildScrollView).last).controller!;
    expect(day.offset, 6.5 * CalendarGrid.hourHeight);
    await tester.pumpWidget(const SizedBox());
  });

  test('up and down step through the events in view, day by day, all-day ones first', () {
    final events = [
      item('Lunch', start: DateTime(2026, 11, 10, 12), end: DateTime(2026, 11, 10, 13)),
      item('Standup', start: DateTime(2026, 11, 10, 9), end: DateTime(2026, 11, 10, 9, 15)),
      item('Trip', startDay: '2026-11-09', endDay: '2026-11-12'),
      item('Next week', start: DateTime(2026, 11, 16, 9), end: DateTime(2026, 11, 16, 10)),
      item('Last week', start: DateTime(2026, 11, 5, 9), end: DateTime(2026, 11, 5, 10)),
    ];
    List<String> titles(CalendarView view, DateTime focus) =>
        [for (final event in eventsInView(events, view, focus, DateTime(2026, 11, 10))) event.title];
    // The trip once, on its first day shown.
    expect(titles(CalendarView.week, DateTime(2026, 11, 11)), ['Trip', 'Standup', 'Lunch']);
    expect(titles(CalendarView.day, DateTime(2026, 11, 10)), ['Trip', 'Standup', 'Lunch']);
    expect(titles(CalendarView.agenda, DateTime(2026, 11, 1)), ['Trip', 'Standup', 'Lunch', 'Next week']);

    final inView = eventsInView(events, CalendarView.week, DateTime(2026, 11, 11), DateTime(2026, 11, 10));
    expect(steppedEvent(inView, null, 1)!.title, 'Trip');
    expect(steppedEvent(inView, null, -1)!.title, 'Lunch');
    expect(steppedEvent(inView, eventKey(inView[1]), 1)!.title, 'Lunch');
    expect(steppedEvent(inView, eventKey(inView[2]), 1)!.title, 'Lunch');
    expect(steppedEvent(inView, eventKey(inView[0]), -1)!.title, 'Trip');
    expect(steppedEvent(const [], null, 1), isNull);
  });
}
