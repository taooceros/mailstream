import 'dart:async';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../platform.dart';
import '../state/calendar_state.dart';
import '../state/mail_state.dart';
import 'calendar_editor.dart';
import 'calendar_grid.dart';
import 'common.dart';
import 'controls.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';

/// An agenda day's events, all-day ones first.
typedef AgendaDay = ({DateTime day, List<engine.CalendarItem> events});

DateTime? _parseDay(String? day) => day == null ? null : DateTime.tryParse(day);

/// When `event` starts and ends, local time; all-day ones at midnight of their days.
({DateTime start, DateTime end}) eventSpan(engine.CalendarItem event) {
  final startDay = _parseDay(event.startDay), endDay = _parseDay(event.endDay);
  if (event.allDay && startDay != null && endDay != null) return (start: startDay, end: endDay);
  return (start: DateTime.parse(event.start).toLocal(), end: DateTime.parse(event.end).toLocal());
}

/// The days from `today` on that have events, each event under every day it covers (a trip on
/// each of its days), earliest first.
List<AgendaDay> agendaDays(List<engine.CalendarItem> events, DateTime today) {
  final byDay = <DateTime, List<engine.CalendarItem>>{};
  for (final event in events) {
    final span = eventSpan(event);
    var day = dayOf(span.start);
    if (day.isBefore(today)) day = today;
    // Ends at midnight: the day before is its last; a moment-long event still shows on its day.
    final last = span.end.isAfter(span.start) ? dayOf(span.end.subtract(const Duration(microseconds: 1))) : dayOf(span.start);
    for (var d = day; !d.isAfter(last); d = DateTime(d.year, d.month, d.day + 1)) {
      (byDay[d] ??= []).add(event);
    }
  }
  final days = byDay.keys.toList()..sort();
  return [
    for (final day in days)
      (
        day: day,
        events: byDay[day]!..sort((a, b) {
          if (a.allDay != b.allDay) return a.allDay ? -1 : 1;
          return eventSpan(a).start.compareTo(eventSpan(b).start);
        }),
      ),
  ];
}

/// The events `view` shows, in the order ↑ and ↓ step through them: the agenda's from `today`
/// on, else the day's or week's around `focus`; day by day, all-day ones first, then by start.
/// An event over several days counts once, on its first day shown.
List<engine.CalendarItem> eventsInView(List<engine.CalendarItem> events, CalendarView view, DateTime focus, DateTime today) {
  final week = weekOf(focus);
  final (from, until) = switch (view) {
    CalendarView.agenda => (today, null),
    CalendarView.day => (focus, DateTime(focus.year, focus.month, focus.day + 1)),
    CalendarView.week => (week, DateTime(week.year, week.month, week.day + 7)),
  };
  final seen = <EventKey>{};
  return [
    for (final day in agendaDays(events, from))
      if (until == null || day.day.isBefore(until))
        for (final event in day.events)
          if (seen.add(eventKey(event))) event,
  ];
}

/// The event `step` after (1) or before (-1) `opened` in `inView` ([eventsInView]), staying at
/// the ends; with none open, or the open one out of view, the first (or the last).
engine.CalendarItem? steppedEvent(List<engine.CalendarItem> inView, EventKey? opened, int step) {
  if (inView.isEmpty) return null;
  final index = opened == null ? -1 : inView.indexWhere((event) => eventKey(event) == opened);
  return inView[index < 0 ? (step > 0 ? 0 : inView.length - 1) : (index + step).clamp(0, inView.length - 1)];
}

/// An agenda day's header: Today, Tomorrow, else the weekday and date.
String agendaTitle(DateTime day, DateTime today) {
  if (sameDay(day, today)) return 'Today · ${DateFormat('EEEE, MMMM d').format(day)}';
  if (sameDay(day, today.add(const Duration(days: 1)))) return 'Tomorrow · ${DateFormat('EEEE, MMMM d').format(day)}';
  if (day.year == today.year) return DateFormat('EEEE, MMMM d').format(day);
  return DateFormat('EEEE, MMMM d, y').format(day);
}

/// An agenda day's header summary: its events, and the hours its timed ones book (overlaps once,
/// declined ones left out): 4 events · 3h 30m.
String daySummary(List<engine.CalendarItem> events, DateTime day) {
  final booked = (bookedHours(layoutDay(events, day)) * 60).round();
  return [events.length == 1 ? '1 event' : '${events.length} events', if (booked > 0) shortDuration(Duration(minutes: booked))].join(' · ');
}

/// How long before a meeting the agenda and the day view offer to join it.
const upNextLead = Duration(minutes: 30);

/// The meeting the Up Next bar offers at `now`: the soonest one starting within [upNextLead],
/// else the one under way that started last. Meetings have a call or guests; all-day and
/// declined events never count.
engine.CalendarItem? upNext(List<engine.CalendarItem> events, DateTime now) {
  engine.CalendarItem? soonest, current;
  DateTime? soonestStart, currentStart;
  for (final event in events) {
    if (event.allDay || event.response == 'declined') continue;
    if (event.meetingUrl == null && event.attendees.isEmpty) continue;
    final span = eventSpan(event);
    if (!span.end.isAfter(now)) continue;
    if (span.start.isAfter(now)) {
      if (span.start.difference(now) > upNextLead) continue;
      if (soonestStart == null || span.start.isBefore(soonestStart)) (soonest, soonestStart) = (event, span.start);
    } else if (currentStart == null || span.start.isAfter(currentStart)) {
      (current, currentStart) = (event, span.start);
    }
  }
  return soonest ?? current;
}

/// When the Up Next meeting starts, from `now`: In 7m (a started minute counts whole), or Now.
String upNextLabel(engine.CalendarItem event, DateTime now) {
  final start = eventSpan(event).start;
  if (!start.isAfter(now)) return 'Now';
  return 'In ${(start.difference(now).inSeconds / 60).ceil()}m';
}

/// The answers an invitation takes, as the engine names them, with their labels.
const replies = [('accepted', 'Going'), ('tentative', 'Maybe'), ('declined', 'Decline')];

/// What the account can do about an event's invitation. [answerable]: it is a guest (not the
/// organizer) and its sign-in may change its calendar; [chosen]: the answer to show picked, the
/// one being sent ([sending]) else the calendar's; [invited]: it has not answered yet, so the
/// agenda's row offers the answers.
typedef ReplyState = ({bool answerable, String? chosen, String? sending, bool invited});

/// `event`'s [ReplyState] for its `account`, with the answer being sent, if any.
ReplyState replyState(engine.CalendarItem event, engine.CalendarAccount? account, String? sending) {
  final response = event.response;
  final guest = response == 'needsAction' || replies.any((r) => r.$1 == response);
  final answerable = guest && account != null && account.granted && account.writable;
  return (
    answerable: answerable,
    chosen: sending ?? (response == 'needsAction' || !guest ? null : response),
    sending: answerable ? sending : null,
    invited: answerable && response == 'needsAction',
  );
}

/// `event`'s answers: the guests, and its organizer first when the guests leave them out.
List<({engine.CalendarPerson person, bool organizer})> eventGuests(engine.CalendarItem event) {
  final organizer = event.organizer;
  final key = organizer?.email?.toLowerCase();
  bool isOrganizer(engine.CalendarPerson person) => key != null && person.email?.toLowerCase() == key;
  return [
    if (organizer != null && !event.attendees.any(isOrganizer)) (person: organizer, organizer: true),
    for (final person in event.attendees) (person: person, organizer: isOrganizer(person)),
  ];
}

/// The guests' answers counted, in the order Going, Maybe, Declined, Invited: 4 Going · 1 Maybe.
String guestSummary(List<engine.CalendarPerson> guests) {
  final counts = <String, int>{};
  for (final person in guests) {
    // No answer yet, or none given: still invited.
    final label = switch (person.response) {
      'accepted' || 'tentative' || 'declined' => _responseLabel(person.response),
      _ => 'Invited',
    };
    counts[label] = (counts[label] ?? 0) + 1;
  }
  return [
    for (final label in const ['Going', 'Maybe', 'Declined', 'Invited'])
      if (counts[label] case final count?) '$count $label',
  ].join(' · ');
}

/// `#rrggbb` as a color.
Color? hexColor(String? hex) {
  if (hex == null || !hex.startsWith('#') || hex.length != 7) return null;
  final value = int.tryParse(hex.substring(1), radix: 16);
  return value == null ? null : Color(0xFF000000 | value);
}

/// The event's color: its calendar's, else its account's.
Color eventColor(MailState state, engine.CalendarItem event) => hexColor(event.color) ?? accountColor(state.accountIndex(event.account));

String _responseLabel(String? response) => switch (response) {
  'accepted' => 'Going',
  'tentative' => 'Maybe',
  'declined' => 'Declined',
  'needsAction' => 'Invited',
  'organizer' => 'Organizer',
  _ => '',
};

/// The account `event` belongs to, as the calendar lists it.
engine.CalendarAccount? _accountOf(CalendarState calendar, engine.CalendarItem event) =>
    calendar.accounts.where((a) => a.id == event.account).firstOrNull;

ReplyState _reply(CalendarState calendar, engine.CalendarItem event) =>
    replyState(event, _accountOf(calendar, event), calendar.answering[eventKey(event)]);

sealed class _Entry {}

final class _Day extends _Entry {
  _Day(this.day, this.title, this.summary);
  final DateTime day;
  final String title;
  final String summary;
}

final class _Event extends _Entry {
  _Event(this.event, this.day);
  final engine.CalendarItem event;
  final DateTime day;
}

final class _Notice extends _Entry {
  _Notice(this.account, {this.problem});
  final engine.CalendarAccount account;

  /// A failed fetch; none: the account must sign in again for calendar access.
  final String? problem;
}

/// The calendar's list pane: header with new event and refresh over the views (agenda, day,
/// week) and their paging, notices for accounts to sign in again or whose fetch failed, then the
/// agenda from today (the week's days on a strip, the meeting about to start, then each day with
/// events under its header) or the day's or week's timeline.
class CalendarListPane extends StatefulWidget {
  const CalendarListPane({super.key, required this.state});

  final MailState state;

  static const rowHeight = 52.0;
  /// A notice card with a two-line message and its [Space.s4] gaps above and below.
  static const noticeHeight = 72.0;

  @override
  State<CalendarListPane> createState() => _CalendarListPaneState();
}

class _CalendarListPaneState extends State<CalendarListPane> {
  static const _listTop = 4.0;

  final _scroll = ScrollController();

  /// The timeline's scroll, kept as Day and Week take turns.
  final _hours = TimelineOffset();

  /// The open event the agenda last brought into view.
  EventKey? _revealed;

  /// The account being signed in to again, while the browser is open.
  String? _signingIn;

  CalendarState get calendar => widget.state.calendar;

  @override
  void initState() {
    super.initState();
    calendar.toNow.addListener(_toTop);
  }

  @override
  void dispose() {
    calendar.toNow.removeListener(_toTop);
    _scroll.dispose();
    super.dispose();
  }

  /// Today in the agenda: back to its top, where today is.
  void _toTop() {
    if (calendar.view == CalendarView.agenda && _scroll.hasClients) {
      _scroll.animateTo(0, duration: Motion.medium, curve: Motion.curve);
    }
  }

  Future<void> _signIn(engine.CalendarAccount account) async {
    setState(() => _signingIn = account.id);
    try {
      await widget.state.addAccount(account.provider);
    } on engine.MailException catch (error) {
      widget.state.showToast('Could not sign in: ${describe(error)}', error: true);
    }
    if (mounted) setState(() => _signingIn = null);
  }

  List<_Notice> _notices() => [
    for (final account in calendar.accounts)
      if (!account.granted)
        _Notice(account)
      else if (account.problem != null)
        _Notice(account, problem: account.problem),
  ];

  Widget _noticeRow(_Notice notice) => _NoticeRow(
    account: notice.account,
    problem: notice.problem,
    signingIn: _signingIn == notice.account.id,
    onSignIn: _signingIn == null ? () => _signIn(notice.account) : null,
  );

  List<_Entry> _entries(DateTime today) {
    final out = <_Entry>[..._notices()];
    for (final day in agendaDays(calendar.events, today)) {
      out.add(_Day(day.day, agendaTitle(day.day, today), daySummary(day.events, day.day)));
      for (final event in day.events) {
        out.add(_Event(event, day.day));
      }
    }
    return out;
  }

  static double _extent(_Entry entry) => switch (entry) {
    _Day() => ListMetrics.sectionHeight,
    _Event() => CalendarListPane.rowHeight,
    _Notice() => CalendarListPane.noticeHeight,
  };

  /// A day on the week strip: the agenda scrolls to it when it lists it, else the day view
  /// shows it (days gone by, days with nothing planned).
  void _showDay(List<_Entry> entries, DateTime day) {
    var offset = _listTop;
    for (final entry in entries) {
      if (entry is _Day && entry.day == day) {
        if (_scroll.hasClients) {
          _scroll.animateTo(offset.clamp(0, _scroll.position.maxScrollExtent), duration: Motion.medium, curve: Motion.curve);
        }
        return;
      }
      offset += _extent(entry);
    }
    calendar.showDay(day);
  }

  /// Brings the open event's row into view when it is off the agenda's visible rows (↑ and ↓,
  /// j and k open the next one): at once, as the event beside it changes at once.
  void _reveal(List<_Entry> entries, EventKey key) {
    if (_scroll.positions.length != 1) return;
    final position = _scroll.position;
    var offset = _listTop;
    for (var i = 0; i < entries.length; i++) {
      final entry = entries[i];
      if (entry is _Event && eventKey(entry.event) == key) {
        // A day's first event brings its header along.
        final top = offset - (i > 0 && entries[i - 1] is _Day ? ListMetrics.sectionHeight : 0);
        final bottom = offset + CalendarListPane.rowHeight;
        if (top < position.pixels) {
          _scroll.jumpTo(top.clamp(position.minScrollExtent, position.maxScrollExtent));
        } else if (bottom > position.pixels + position.viewportDimension) {
          _scroll.jumpTo((bottom - position.viewportDimension).clamp(position.minScrollExtent, position.maxScrollExtent));
        }
        return;
      }
      offset += _extent(entry);
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final now = DateTime.now();
    final today = dayOf(now);
    final view = calendar.view;
    final Widget body;
    void open(engine.CalendarItem event) => calendar.open(eventKey(event));
    final upNextBar = _UpNextBar(key: const ValueKey('upNext'), state: widget.state, onOpen: open);
    if (!calendar.listed) {
      body = const Center(child: Spinner());
    } else if (view != CalendarView.agenda) {
      final days = view == CalendarView.week ? weekDays(weekOf(calendar.focus)) : [calendar.focus];
      final notices = _notices();
      body = Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // One line, leaving the hours their room: the agenda has each account's notice.
          if (notices.isNotEmpty) _NoticeLine(notices: notices, onTap: () => calendar.setView(CalendarView.agenda)),
          if (view == CalendarView.day) upNextBar,
          // Keyed: the notice line and the Up Next bar come and go above it without starting the
          // grid over.
          Expanded(
            key: const ValueKey('grid'),
            child: CalendarGrid(
              days: days,
              events: calendar.events,
              colorOf: (event) => eventColor(widget.state, event),
              photoOf: widget.state.avatarFor,
              photosVersion: widget.state.avatarsVersion,
              synced: calendar.synced,
              selected: calendar.opened,
              onOpen: open,
              onDay: view == CalendarView.week ? calendar.showDay : null,
              onCreate: calendar.writableAccounts.isEmpty ? null : (start) => showEventEditor(context, widget.state, start: start),
              onPage: calendar.page,
              toNow: calendar.toNow,
              offset: _hours,
            ),
          ),
        ],
      );
    } else {
      final entries = _entries(today);
      final opened = calendar.opened;
      if (opened != _revealed) {
        _revealed = opened;
        if (opened != null) WidgetsBinding.instance.addPostFrameCallback((_) => _reveal(entries, opened));
      }
      final hasEvents = entries.any((e) => e is _Event);
      final empty = hasEvents
          ? null
          : calendar.hasCalendars
          ? (calendar.loading ? null : 'Nothing on your calendars in the next ${CalendarState.days} days')
          : 'No account has calendar access yet. Sign in again to show its calendar here.';
      body = Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _WeekStrip(events: calendar.events, today: today, onDay: (day) => _showDay(entries, day)),
          upNextBar,
          Expanded(
            // MacosApp's scroll behavior puts the Mac's overlay scrollbar on it.
            child: ListView.builder(
              controller: _scroll,
              padding: const EdgeInsets.only(top: _listTop, bottom: 24),
              itemCount: entries.length + 1,
              itemExtentBuilder: (index, _) =>
                  index == entries.length ? (empty == null && hasEvents ? 0 : 120) : _extent(entries[index]),
              itemBuilder: (context, index) {
                if (index == entries.length) {
                  if (hasEvents) return const SizedBox.shrink();
                  if (empty == null) return const Center(child: Spinner());
                  return Center(
                    child: Padding(
                      padding: const EdgeInsets.all(24),
                      child: Text(empty, textAlign: TextAlign.center, style: context.text.body.copyWith(color: colors.secondaryLabel)),
                    ),
                  );
                }
                return switch (entries[index]) {
                  _Day(:final title, :final summary) => _DayHeader(title: title, summary: summary),
                  _Event(:final event, :final day) => _EventRow(
                    key: ValueKey('${event.account}|${event.calendar}|${event.id}|${day.millisecondsSinceEpoch}'),
                    state: widget.state,
                    event: event,
                    day: day,
                    now: now,
                    selected: calendar.opened == eventKey(event),
                    onTap: () => open(event),
                  ),
                  final _Notice notice => _noticeRow(notice),
                };
              },
            ),
          ),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(18, 0, 12, 0),
            child: Column(children: [
              SizedBox(
                height: 52,
                child: Row(children: [
                  Expanded(
                    child: Column(
                      mainAxisAlignment: MainAxisAlignment.center,
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(children: [
                          Text('Calendar', style: context.text.title3.copyWith(fontWeight: FontWeight.w700)),
                          if (calendar.loading) const Padding(padding: EdgeInsets.only(left: 8), child: Spinner(radius: 6)),
                        ]),
                        Text(
                          switch (view) {
                            CalendarView.agenda => DateFormat('EEEE, MMMM d').format(now),
                            CalendarView.day => dayTitle(calendar.focus),
                            CalendarView.week => weekTitle(weekOf(calendar.focus)),
                          },
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: context.text.subheadline.copyWith(color: colors.secondaryLabel),
                        ),
                      ],
                    ),
                  ),
                  GlassCapsule(
                    child: Row(mainAxisSize: MainAxisSize.min, children: [
                      ToolButton(
                        tooltip: 'New event (${Host.shortcut('N')})',
                        icon: CupertinoIcons.plus,
                        onPressed: calendar.writableAccounts.isEmpty ? null : () => showEventEditor(context, widget.state),
                      ),
                      ToolButton(
                        tooltip: 'Refresh (${Host.shortcut('R')})',
                        icon: CupertinoIcons.arrow_clockwise,
                        onPressed: calendar.hasCalendars ? calendar.refresh : null,
                      ),
                    ]),
                  ),
                ]),
              ),
              Padding(padding: const EdgeInsets.only(bottom: 8), child: CalendarViewBar(calendar: calendar)),
            ]),
          ),
        ),
        Expanded(
          child: ClipRect(
            // Agenda, Day and Week fade and grow into each other.
            child: AnimatedSwitcher(
              duration: Motion.short,
              switchInCurve: Motion.curve,
              switchOutCurve: Motion.curve,
              layoutBuilder: (current, previous) => Stack(fit: StackFit.expand, children: [...previous, ?current]),
              transitionBuilder: (child, animation) => FadeTransition(
                opacity: animation,
                child: ScaleTransition(scale: Tween(begin: Motion.enterScale, end: 1.0).animate(animation), child: child),
              ),
              child: KeyedSubtree(key: ValueKey(calendar.listed ? view : null), child: body),
            ),
          ),
        ),
      ],
    );
  }
}

/// Tabular figures, so times line up down the agenda's time column.
const _tabular = [FontFeature.tabularFigures()];

/// The agenda's week strip: this week's seven days as the week view heads them (today raised on
/// a card, the others over a bar of the hours they book). A day scrolls the agenda to it, or
/// opens it in the day view when the agenda does not list it.
class _WeekStrip extends StatelessWidget {
  const _WeekStrip({required this.events, required this.today, required this.onDay});

  final List<engine.CalendarItem> events;
  final DateTime today;
  final ValueChanged<DateTime> onDay;

  static const _height = 44.0;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(10, 0, 10, 8),
    child: SizedBox(
      height: _height,
      child: Row(children: [
        for (final day in weekDays(weekOf(today)))
          Expanded(
            child: CalendarDayHeader(day: day, today: day == today, booked: bookedHours(layoutDay(events, day)), onTap: onDay),
          ),
      ]),
    ),
  );
}

/// The meeting about to start (within [upNextLead]) or under way, on one compact card: how soon,
/// its title, and Join when it has a call; a click opens it. It follows the clock by itself.
class _UpNextBar extends StatefulWidget {
  const _UpNextBar({super.key, required this.state, required this.onOpen});

  final MailState state;
  final ValueChanged<engine.CalendarItem> onOpen;

  @override
  State<_UpNextBar> createState() => _UpNextBarState();
}

class _UpNextBarState extends State<_UpNextBar> {
  Timer? _clock;

  /// What the bar last showed: its event and label; the clock rebuilds it when they change.
  (EventKey, String)? _shown;

  @override
  void initState() {
    super.initState();
    _clock = Timer.periodic(const Duration(seconds: 15), (_) {
      if (_current(DateTime.now()) != _shown) setState(() {});
    });
  }

  @override
  void dispose() {
    _clock?.cancel();
    super.dispose();
  }

  (EventKey, String)? _current(DateTime now) {
    final event = upNext(widget.state.calendar.events, now);
    return event == null ? null : (eventKey(event), upNextLabel(event, now));
  }

  @override
  Widget build(BuildContext context) {
    final now = DateTime.now();
    final event = upNext(widget.state.calendar.events, now);
    _shown = event == null ? null : (eventKey(event), upNextLabel(event, now));
    if (event == null) return const SizedBox.shrink();
    final colors = context.colors;
    final url = event.meetingUrl;
    const radius = BorderRadius.all(Radius.circular(CalendarMetrics.cardRadius));
    final content = SizedBox(
      height: 36,
      child: Row(children: [
        const SizedBox(width: 12),
        MarkDot(colors.accent),
        const SizedBox(width: 8),
        Text(
          _shown!.$2,
          style: context.text.callout.copyWith(color: colors.accent, fontWeight: FontWeight.w600, fontFeatures: _tabular),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            event.title,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: context.text.callout.copyWith(color: colors.label, fontWeight: FontWeight.w600),
          ),
        ),
        if (url != null) ...[
          const SizedBox(width: 8),
          MailButton(
            label: 'Join',
            icon: CupertinoIcons.videocam_fill,
            kind: MailButtonKind.primary,
            size: MailControlSize.small,
            onPressed: () => widget.state.openExternal(url),
          ),
        ],
        const SizedBox(width: 8),
      ]),
    );
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      // A card on the tray like the timeline's events, half lifted under the pointer.
      child: Liftable(
        onTap: () => widget.onOpen(event),
        builder: (context, half) => DecoratedBox(
          decoration: half
              ? LiftedCard.half(colors, radius, face: colors.card)
              : BoxDecoration(
                  color: colors.card,
                  borderRadius: radius,
                  border: Border.all(color: colors.cardBorder, width: CalendarMetrics.cardBorder),
                  boxShadow: colors.cardShadow,
                ),
          child: content,
        ),
      ),
    );
  }
}

/// An agenda day's header: its title, and how many events it has and the time they book.
class _DayHeader extends StatelessWidget {
  const _DayHeader({required this.title, required this.summary});

  final String title;
  final String summary;

  @override
  Widget build(BuildContext context) {
    final style = context.text.callout.copyWith(color: context.colors.secondaryLabel, fontWeight: FontWeight.w600);
    return Container(
      alignment: Alignment.bottomLeft,
      padding: const EdgeInsets.fromLTRB(22, 0, 18, 5),
      child: Row(crossAxisAlignment: CrossAxisAlignment.end, children: [
        Expanded(child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: style)),
        const SizedBox(width: 8),
        Text(summary, style: context.text.footnote.copyWith(color: context.colors.secondaryLabel, fontFeatures: _tabular)),
      ]),
    );
  }
}

/// One event in the agenda: a time column (when it starts and how long it runs, or All day),
/// its calendar's dot, the title and where it is, then its guests' avatars, or the answers
/// when the account is invited and has not answered. Past events fade; declined ones fade
/// further, struck through. Selected, the row lifts as a card with a blue mark at its edge.
class _EventRow extends StatelessWidget {
  const _EventRow({
    super.key,
    required this.state,
    required this.event,
    required this.day,
    required this.now,
    required this.selected,
    required this.onTap,
  });

  final MailState state;
  final engine.CalendarItem event;
  final DateTime day;
  final DateTime now;
  final bool selected;
  final VoidCallback onTap;

  static const _timeWidth = 64.0;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final calendar = state.calendar;
    final span = eventSpan(event);
    final past = !span.end.isAfter(now);
    final declined = event.response == 'declined';
    // A timed event shows its times on the day it starts; days it runs through show All day, and
    // the day it ends, when it ends.
    final startsToday = !event.allDay && sameDay(span.start, day);
    final endsToday = !event.allDay && (sameDay(span.end, day) || span.end == DateTime(day.year, day.month, day.day + 1));
    final time = startsToday ? DateFormat.jm().format(span.start) : 'All day';
    final under = startsToday && endsToday
        ? shortDuration(span.end.difference(span.start))
        : endsToday && !startsToday
        ? 'until ${DateFormat.jm().format(span.end)}'
        : null;
    final detail = [
      if (event.location != null) event.location!,
      if (event.meetingUrl != null) meetingLabel(event.meetingUrl!),
      if (calendar.accounts.where((a) => a.granted).length > 1 || (event.calendarName != null && event.calendarName != event.account))
        event.calendarName ?? event.account,
    ].join(' · ');
    final reply = _reply(calendar, event);
    final tag = switch (event.response) {
      'needsAction' => 'Invited',
      'tentative' => 'Maybe',
      _ => null,
    };
    final secondary = colors.secondaryLabel;
    final timeStyle = context.text.callout.copyWith(color: colors.label, fontWeight: FontWeight.w600, fontFeatures: _tabular);
    final guests = eventGuests(event);
    final Widget? trailing = reply.invited
        ? _ReplySwitch(reply: reply, compact: true, onReply: (response) => calendar.respond(event, response))
        : guests.isNotEmpty
        ? GuestAvatars(event: event, max: 3, photoOf: state.avatarFor)
        : null;
    Widget row = Row(children: [
      SizedBox(
        width: _timeWidth,
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Text(time, maxLines: 1, style: timeStyle),
            if (under != null)
              Text(under, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.footnote.copyWith(color: secondary, fontFeatures: _tabular)),
          ],
        ),
      ),
      const SizedBox(width: 12),
      MarkDot(calendarDot(eventColor(state, event), colors)),
      const SizedBox(width: 8),
      Expanded(
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              event.title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: context.text.body.copyWith(
                fontWeight: FontWeight.w600,
                color: colors.label,
                decoration: declined ? TextDecoration.lineThrough : null,
              ),
            ),
            if (detail.isNotEmpty || tag != null)
              Row(children: [
                if (tag != null) ...[
                  Tag(label: tag, color: colors.waiting, compact: true),
                  if (detail.isNotEmpty) const SizedBox(width: 6),
                ],
                if (event.location != null || event.meetingUrl != null) ...[
                  Icon(event.location != null ? CupertinoIcons.location : CupertinoIcons.videocam, size: 12, color: secondary),
                  const SizedBox(width: 3),
                ],
                Expanded(child: Text(detail, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.callout.copyWith(color: secondary))),
              ]),
          ],
        ),
      ),
      if (trailing != null) ...[const SizedBox(width: 8), trailing],
    ]);
    if (past || declined) row = Opacity(opacity: declined ? CalendarMetrics.declinedOpacity : CalendarMetrics.pastOpacity, child: row);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset),
      child: LiftedRow(
        selected: selected,
        onTap: onTap,
        child: Padding(padding: const EdgeInsets.only(left: 4, right: 12), child: row),
      ),
    );
  }
}

/// Going, Maybe and Decline side by side, the account's answer raised and checked; the answer
/// being sent spins. [compact] for the agenda's rows.
class _ReplySwitch extends StatelessWidget {
  const _ReplySwitch({required this.reply, required this.onReply, this.compact = false});

  final ReplyState reply;
  final ValueChanged<String> onReply;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return MailSegmented<String>(
      compact: compact,
      selected: reply.chosen,
      onChanged: reply.answerable && reply.sending == null ? onReply : null,
      segments: [
        for (final (value, label) in replies)
          MailSegment(
            value,
            label,
            tint: value == 'declined' ? colors.red : colors.accent,
            leading: reply.sending == value
                ? const Spinner(radius: 5)
                : reply.chosen == value
                ? Icon(CupertinoIcons.checkmark, size: compact ? 10 : 12, color: value == 'declined' ? colors.red : colors.accent)
                : null,
          ),
      ],
    );
  }
}

/// An account whose calendar cannot show: it must sign in again for calendar access, or its
/// last fetch failed.
class _NoticeRow extends StatelessWidget {
  const _NoticeRow({required this.account, required this.problem, required this.signingIn, required this.onSignIn});

  final engine.CalendarAccount account;
  final String? problem;
  final bool signingIn;
  final VoidCallback? onSignIn;

  @override
  Widget build(BuildContext context) {
    final failed = problem != null;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: Space.s12, vertical: Space.s4),
      child: MailNotice(
        tone: failed ? MailNoticeTone.error : MailNoticeTone.action,
        icon: failed ? null : CupertinoIcons.calendar_badge_plus,
        title: account.label.isEmpty ? account.id : account.label,
        message: failed ? 'Could not update: $problem' : 'Sign in again to allow calendar access.',
        trailing: failed
            ? null
            : Row(mainAxisSize: MainAxisSize.min, children: [
                if (signingIn) const Padding(padding: EdgeInsets.only(right: Space.s8), child: Spinner(radius: 7)),
                MailButton(label: 'Sign In…', onPressed: onSignIn),
              ]),
      ),
    );
  }
}

/// The day and week views' one line for the accounts whose calendar cannot show (to sign in
/// again, or whose fetch failed); a click opens the agenda, which has each one's notice.
class _NoticeLine extends StatelessWidget {
  const _NoticeLine({required this.notices, required this.onTap});

  final List<_Notice> notices;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final failed = notices.any((n) => n.problem != null);
    final only = notices.length == 1 ? notices.single.account : null;
    final text = only != null
        ? '${only.label.isEmpty ? only.id : only.label}: ${failed ? 'could not update' : 'sign in again to show its calendar'}'
        : '${notices.length} accounts cannot show their calendars';
    return Padding(
      padding: const EdgeInsets.fromLTRB(Space.s12, 0, Space.s12, Space.s6),
      child: MailNotice(
        compact: true,
        tone: failed ? MailNoticeTone.error : MailNoticeTone.action,
        icon: failed ? null : CupertinoIcons.calendar_badge_plus,
        message: text,
        action: 'Show in Agenda',
        onTap: onTap,
      ),
    );
  }
}

/// The open event: its calendar, title and when; a bar answering the invitation inline (Going,
/// Maybe, Decline) beside Join for its call; where; the guests two to a row with their answers;
/// its description, and the provider's page for it.
class CalendarEventPane extends StatelessWidget {
  const CalendarEventPane({super.key, required this.state, required this.event, required this.twoPane, required this.onBack});

  final MailState state;
  final engine.CalendarItem event;
  final bool twoPane;
  final VoidCallback onBack;

  /// Guest cards go two to a row from this width.
  static const _twoColumns = 320.0;

  String _when(DateTime now) {
    final span = eventSpan(event);
    String day(DateTime d) => DateFormat(d.year == now.year ? 'EEEE, MMMM d' : 'EEEE, MMMM d, y').format(d);
    if (event.allDay) {
      final last = span.end.subtract(const Duration(days: 1));
      return !last.isAfter(span.start) ? '${day(span.start)} · All day' : '${day(span.start)} – ${day(last)}';
    }
    final time = DateFormat.jm();
    if (sameDay(span.start, span.end)) return '${day(span.start)} · ${time.format(span.start)} – ${time.format(span.end)}';
    return '${day(span.start)}, ${time.format(span.start)} – ${day(span.end)}, ${time.format(span.end)}';
  }

  /// The bar under the title: the answers (or the account's part, when it cannot answer from
  /// here) and Join; none when there is neither.
  Widget? _actionBar(BuildContext context) {
    final colors = context.colors;
    final calendar = state.calendar;
    final reply = _reply(calendar, event);
    final url = event.meetingUrl;
    final part = switch (event.response) {
      'organizer' => 'You organize this',
      final response? when !reply.answerable => _responseLabel(response),
      _ => '',
    };
    if (!reply.answerable && part.isEmpty && url == null) return null;
    return Container(
      padding: const EdgeInsets.all(6),
      decoration: BoxDecoration(color: colors.fill, borderRadius: BorderRadius.circular(CalendarMetrics.cardRadius + 2)),
      child: Row(children: [
        if (reply.answerable)
          _ReplySwitch(reply: reply, onReply: (response) => calendar.respond(event, response))
        else if (part.isNotEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 6),
            child: Text(part, style: context.text.body.copyWith(color: colors.secondaryLabel, fontWeight: FontWeight.w500)),
          ),
        const Spacer(),
        if (url != null)
          MailButton(
            label: 'Join',
            icon: CupertinoIcons.videocam_fill,
            kind: MailButtonKind.primary,
            onPressed: () => state.openExternal(url),
          ),
      ]),
    );
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final provider = _accountOf(state.calendar, event)?.provider;
    Widget line(IconData icon, Widget child) => Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Padding(padding: const EdgeInsets.only(top: 1), child: Icon(icon, size: 16, color: colors.secondaryLabel)),
        const SizedBox(width: 10),
        Expanded(child: child),
      ]),
    );
    final text = context.text.body;
    final actions = _actionBar(context);
    final guests = eventGuests(event);
    return DetailPane(
      toolbar: DetailToolbar(
        leading: CloseCapsule(twoPane: twoPane, onPressed: onBack),
        actions: [
          if (event.editable)
            GlassCapsule(
              child: Row(mainAxisSize: MainAxisSize.min, children: [
                ToolButton(
                  tooltip: 'Edit',
                  icon: CupertinoIcons.pencil,
                  onPressed: () => showEventEditor(context, state, event: event),
                ),
                ToolButton(
                  tooltip: 'Delete',
                  icon: CupertinoIcons.trash,
                  onPressed: () => confirmDeleteEvent(context, state, event),
                ),
              ]),
            ),
          if (event.link != null)
            GlassCapsule(
              child: ToolButton(
                tooltip: provider == 'microsoft' ? 'Open in Outlook' : 'Open in Google Calendar',
                icon: CupertinoIcons.arrow_up_right_square,
                onPressed: () => state.openExternal(event.link!),
              ),
            ),
        ],
      ),
      // The event scrolls under the toolbar, as a conversation does.
      content: Selectable(
        child: ListView(
          padding: const EdgeInsets.fromLTRB(Space.s24, DetailToolbar.height + Space.s8, Space.s24, Space.s24),
          children: [
            Wrap(spacing: 6, runSpacing: 6, children: [
              MarkChip(
                label: [event.calendarName ?? event.calendar, if (event.calendarName != event.account) event.account].join(' · '),
                mark: calendarDot(eventColor(state, event), colors),
              ),
              if (event.series != null) const Pill(label: 'Repeats'),
            ]),
            const SizedBox(height: 10),
            Text(
              event.title,
              style: context.roles.display.copyWith(decoration: event.response == 'declined' ? TextDecoration.lineThrough : null),
            ),
            const SizedBox(height: 6),
            line(CupertinoIcons.clock, Text(_when(DateTime.now()), style: text.copyWith(color: colors.secondaryLabel))),
            if (actions != null) Padding(padding: const EdgeInsets.only(top: 4, bottom: 14), child: actions),
            if (event.location != null) line(CupertinoIcons.location, Text(event.location!, style: text)),
            if (guests.isNotEmpty) ...[
              const SizedBox(height: 4),
              Row(children: [
                Text(guests.length == 1 ? '1 guest' : '${guests.length} guests', style: text.copyWith(fontWeight: FontWeight.w600)),
                const Spacer(),
                Text(guestSummary(event.attendees), style: context.text.footnote.copyWith(color: colors.secondaryLabel)),
              ]),
              const SizedBox(height: 8),
              LayoutBuilder(builder: (context, constraints) {
                const gap = 8.0;
                final columns = constraints.maxWidth >= _twoColumns ? 2 : 1;
                final width = (constraints.maxWidth - gap * (columns - 1)) / columns;
                return Wrap(spacing: gap, runSpacing: gap, children: [
                  for (final guest in guests)
                    SizedBox(width: width, child: _GuestCard(person: guest.person, organizer: guest.organizer, photo: guest.person.email == null ? null : state.avatarFor(guest.person.email!))),
                ]);
              }),
              const SizedBox(height: 14),
            ],
            if (event.notes != null) ...[
              const Hairline(),
              const SizedBox(height: 14),
              Text('Notes', style: text.copyWith(fontWeight: FontWeight.w600)),
              const SizedBox(height: 6),
              Text(event.notes!, style: text),
            ],
          ],
        ),
      ),
    );
  }
}

/// A guest on the open event: their avatar, name, and part (Organizer) and answer; the
/// answers still owed (Maybe, Invited) in the waiting color.
class _GuestCard extends StatelessWidget {
  const _GuestCard({required this.person, required this.organizer, required this.photo});

  final engine.CalendarPerson person;
  final bool organizer;
  final String? photo;

  static const _avatar = 26.0;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final name = person.name ?? person.email ?? 'Someone';
    final answer = _responseLabel(person.response);
    final status = [if (organizer) 'Organizer', if (answer.isNotEmpty && answer != 'Organizer') answer].join(' · ');
    final owed = person.response == 'tentative' || person.response == 'needsAction';
    final card = Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
      decoration: BoxDecoration(color: colors.fill, borderRadius: BorderRadius.circular(CalendarMetrics.cardRadius)),
      child: Row(children: [
        PersonAvatar(address: guestAddress(person), photo: photo, size: _avatar),
        const SizedBox(width: 8),
        Expanded(
          child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            Text(name, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.callout.copyWith(color: colors.label, fontWeight: FontWeight.w600)),
            if (status.isNotEmpty)
              Text(status, maxLines: 1, overflow: TextOverflow.ellipsis,
                  style: context.text.footnote.copyWith(color: owed ? colors.waiting : colors.secondaryLabel)),
          ]),
        ),
      ]),
    );
    final email = person.email;
    return email == null || email == name ? card : MacosTooltip(message: email, child: card);
  }
}

