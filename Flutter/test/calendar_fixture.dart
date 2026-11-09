import 'package:mailstream/mailstream.dart' as engine;

import 'reader_fixture.dart';

const me = 'me@example.test';
const team = 'team@example.test';

engine.CalendarPerson guest(String name, String? response) =>
    engine.CalendarPerson(name: name, email: '${name.split(' ').first.toLowerCase()}@example.test', response: response);

/// A synthetic calendar around `now`, for the agenda, its Up Next bar and the event pane: a
/// Google account (Work and Personal) and an Outlook one (Team), both allowed to answer. A
/// meeting starts seven minutes after `now`; one ended an hour ago; two invitations wait for an
/// answer, one is tentative and one declined. Answers stay in memory: the event takes the answer
/// after [answerDelay] and the calendar changes, as the engine does, and nothing is sent.
class CalendarMail extends ReaderMail {
  CalendarMail({DateTime? now, this.answerDelay = Duration.zero}) : now = now ?? DateTime.now() {
    events = _events(this.now);
  }

  final DateTime now;
  final Duration answerDelay;
  late List<engine.CalendarItem> events;

  /// The answers asked for: account, calendar, event, answer, confirmed.
  final answers = <(String, String?, String, String, bool)>[];

  static engine.CalendarItem event(
    String id,
    String title,
    DateTime start,
    Duration length, {
    String account = me,
    String calendar = 'work',
    String? response,
    String? location,
    String? meetingUrl,
    engine.CalendarPerson? organizer,
    List<engine.CalendarPerson> attendees = const [],
    String? notes,
    bool allDay = false,
  }) {
    final end = start.add(length);
    String day(DateTime d) => '${d.year}-${d.month.toString().padLeft(2, '0')}-${d.day.toString().padLeft(2, '0')}';
    return engine.CalendarItem(
      account: account,
      calendar: calendar,
      calendarName: switch (calendar) {
        'work' => 'Work',
        'personal' => 'Personal',
        _ => 'Team',
      },
      color: switch (calendar) {
        'work' => '#4285f4',
        'personal' => '#33b679',
        _ => '#e67c73',
      },
      id: id,
      title: title,
      start: start.toUtc().toIso8601String(),
      end: end.toUtc().toIso8601String(),
      allDay: allDay,
      startDay: allDay ? day(start) : null,
      endDay: allDay ? day(end) : null,
      location: location,
      meetingUrl: meetingUrl,
      organizer: organizer,
      attendees: attendees,
      response: response,
      notes: notes,
      notesCut: false,
      link: null,
      series: null,
      editable: response == 'organizer',
    );
  }

  static List<engine.CalendarItem> _events(DateTime now) {
    final minute = DateTime(now.year, now.month, now.day, now.hour, now.minute);
    DateTime day(int days, int hour, [int minutes = 0]) => DateTime(now.year, now.month, now.day + days, hour, minutes);
    final alex = guest('Alex Rivera', 'accepted');
    return [
      event('triage', 'Sprint Triage & Bug Scrub', minute.subtract(const Duration(hours: 2)), const Duration(minutes: 45),
          response: 'accepted', meetingUrl: 'https://meet.example.test/triage', organizer: alex,
          attendees: [alex, guest('Maya Chen', 'accepted'), guest('Me', 'accepted')]),
      event('review', 'Architecture Review', minute.add(const Duration(minutes: 7)), const Duration(minutes: 75),
          response: 'accepted',
          location: 'Building 4 · Room 4B',
          meetingUrl: 'https://meet.example.test/review',
          organizer: alex,
          attendees: [
            alex,
            guest('Maya Chen', 'accepted'),
            guest('Liam Park', 'accepted'),
            guest('Sofia Patel', 'tentative'),
            guest('Me', 'accepted'),
            guest('Noah Kim', 'needsAction'),
          ],
          notes: 'Review the WAL snapshot handoff between the engine and the Flutter isolate; check 120 Hz pacing.'),
      event('vendor', 'Vendor Demo: Vector Index', minute.add(const Duration(hours: 3)), const Duration(minutes: 30),
          account: team, calendar: 'team', response: 'needsAction', meetingUrl: 'https://teams.example.test/vendor',
          organizer: guest('Priya Shah', 'accepted'), attendees: [guest('Priya Shah', 'accepted'), guest('Team', 'needsAction')]),
      event('polish', 'Design System Polish', minute.add(const Duration(hours: 5)), const Duration(hours: 1),
          calendar: 'personal', response: 'organizer', location: 'Studio B', organizer: guest('Me', null),
          attendees: [guest('Emma Stone', 'accepted'), guest('Sam Lee', 'tentative')]),
      event('handoff', 'Weekly Release Handoff', day(1, 9, 30), const Duration(hours: 1),
          response: 'tentative', meetingUrl: 'https://meet.example.test/handoff', organizer: alex, attendees: [alex, guest('Me', 'tentative')]),
      event('offsite', 'Offsite Logistics', day(1, 13), const Duration(minutes: 45),
          response: 'declined', organizer: guest('Liam Park', 'accepted'), attendees: [guest('Liam Park', 'accepted'), guest('Me', 'declined')]),
      event('hiring', 'Hiring Sync', day(2, 11), const Duration(minutes: 30),
          response: 'needsAction', meetingUrl: 'https://meet.example.test/hiring', organizer: guest('Maya Chen', 'accepted'),
          attendees: [guest('Maya Chen', 'accepted'), guest('Liam Park', 'needsAction'), guest('Me', 'needsAction')]),
      event('trip', 'Conference Trip · San Francisco', day(2, 0), const Duration(days: 3), calendar: 'personal', allDay: true),
      event('gym', 'Gym', day(3, 18), const Duration(hours: 1), calendar: 'personal'),
    ];
  }

  static engine.CalendarAccount _account(String id, String label, String provider, List<engine.CalendarInfo> calendars) =>
      engine.CalendarAccount(
        id: id,
        label: label,
        provider: provider,
        granted: true,
        writable: true,
        loading: false,
        problem: null,
        fetched: null,
        calendars: calendars,
      );

  @override
  Future<engine.CalendarAgenda> calendarEvents({required String? from, required int days}) async => engine.CalendarAgenda(
    accounts: [
      _account(me, 'Work', 'google', [
        engine.CalendarInfo(id: 'work', name: 'Work', color: '#4285f4', primary: true, writable: true),
        engine.CalendarInfo(id: 'personal', name: 'Personal', color: '#33b679', primary: false, writable: true),
      ]),
      _account(team, 'Team', 'microsoft', [engine.CalendarInfo(id: 'team', name: 'Team', color: '#e67c73', primary: true, writable: true)]),
    ],
    loading: false,
    events: events,
    timeZone: 'America/Los_Angeles',
    synced: engine.CalendarDays(from: '2026-01-01', until: '2028-01-01'),
  );

  @override
  Future<void> calendarRespond({
    required String account,
    required String? calendar,
    required String event,
    required String response,
    required bool confirm,
  }) async {
    answers.add((account, calendar, event, response, confirm));
    if (answerDelay > Duration.zero) await Future<void>.delayed(answerDelay);
    events = [
      for (final e in events)
        if (e.account == account && e.id == event) _answered(e, response) else e,
    ];
    changed(engine.CalendarEvent());
  }

  static engine.CalendarItem _answered(engine.CalendarItem e, String response) => engine.CalendarItem(
    account: e.account,
    calendar: e.calendar,
    calendarName: e.calendarName,
    color: e.color,
    id: e.id,
    title: e.title,
    start: e.start,
    end: e.end,
    allDay: e.allDay,
    startDay: e.startDay,
    endDay: e.endDay,
    location: e.location,
    meetingUrl: e.meetingUrl,
    organizer: e.organizer,
    attendees: e.attendees,
    response: response,
    notes: e.notes,
    notesCut: e.notesCut,
    link: e.link,
    series: e.series,
    editable: e.editable,
  );
}

