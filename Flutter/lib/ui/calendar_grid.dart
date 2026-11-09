import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/foundation.dart' show ValueListenable, listEquals;
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart' hide TextDirection;
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart' show Address;
import '../bench.dart';
import '../state/calendar_state.dart';
import 'calendar.dart';
import 'common.dart';
import 'design.dart';
import 'glass.dart';

/// Minutes in a day by the clock.
const _dayMinutes = 24 * 60;

/// The shortest an event is drawn, so a short one still has room for its title.
const minEventMinutes = 20;

/// A timed event's stretch of one day, in minutes from that day's midnight by the clock
/// (`bottom` excluded), and where it sits among the events it overlaps: in `column` of `columns`,
/// `span` columns wide.
typedef DayPlacement = ({engine.CalendarItem event, int top, int bottom, int column, int columns, int span});

/// An event in the all-day row: from day `first` to day `last` (indices into the days shown), in
/// `row`.
typedef AllDayPlacement = ({engine.CalendarItem event, int first, int last, int row});

/// Whether `event` goes in the all-day row over the hours: all-day events and those lasting a
/// day or more.
bool inAllDayRow(engine.CalendarItem event) {
  if (event.allDay) return true;
  final span = eventSpan(event);
  return span.end.difference(span.start) >= const Duration(hours: 24);
}

/// Minutes since midnight by the clock, so a day with a daylight-saving change still has its
/// hours where the clock shows them.
int _clockMinutes(DateTime time) => time.hour * 60 + time.minute;

/// The timed events on `day`, laid out side by side where they overlap: earliest first (the
/// longer of two starting together first), each overlapping group sharing the width, each event
/// in the first column free when it starts and widened over the columns to its right that stay
/// free while it lasts. An event over midnight is drawn on each of its days.
List<DayPlacement> layoutDay(List<engine.CalendarItem> events, DateTime day) {
  final next = DateTime(day.year, day.month, day.day + 1);
  final pieces = <({engine.CalendarItem event, int top, int bottom})>[];
  for (final event in events) {
    if (inAllDayRow(event)) continue;
    final span = eventSpan(event);
    final moment = !span.end.isAfter(span.start);
    final onDay = moment
        ? !span.start.isBefore(day) && span.start.isBefore(next)
        : span.end.isAfter(day) && span.start.isBefore(next);
    if (!onDay) continue;
    final top = math.min(span.start.isBefore(day) ? 0 : _clockMinutes(span.start), _dayMinutes - minEventMinutes);
    final end = span.end.isBefore(next) ? _clockMinutes(span.end) : _dayMinutes;
    pieces.add((event: event, top: top, bottom: math.min(math.max(end, top + minEventMinutes), _dayMinutes)));
  }
  pieces.sort((a, b) {
    if (a.top != b.top) return a.top.compareTo(b.top);
    if (a.bottom != b.bottom) return b.bottom.compareTo(a.bottom);
    return a.event.title.compareTo(b.event.title);
  });

  final out = <DayPlacement>[];
  var group = <(({engine.CalendarItem event, int top, int bottom}), int)>[];
  var columnEnds = <int>[];
  var groupEnd = 0;
  void flush() {
    final columns = columnEnds.length;
    for (final (piece, column) in group) {
      var span = 1;
      while (column + span < columns &&
          !group.any((other) => other.$2 == column + span && other.$1.top < piece.bottom && other.$1.bottom > piece.top)) {
        span++;
      }
      out.add((event: piece.event, top: piece.top, bottom: piece.bottom, column: column, columns: columns, span: span));
    }
    group = [];
    columnEnds = [];
  }

  for (final piece in pieces) {
    if (group.isNotEmpty && piece.top >= groupEnd) flush();
    var column = columnEnds.indexWhere((end) => end <= piece.top);
    if (column < 0) {
      column = columnEnds.length;
      columnEnds.add(piece.bottom);
    } else {
      columnEnds[column] = piece.bottom;
    }
    group.add((piece, column));
    groupEnd = group.length == 1 ? piece.bottom : math.max(groupEnd, piece.bottom);
  }
  if (group.isNotEmpty) flush();
  return out;
}

/// Days between two local midnights, whatever daylight saving does between them.
int _daysBetween(DateTime from, DateTime to) =>
    DateTime.utc(to.year, to.month, to.day).difference(DateTime.utc(from.year, from.month, from.day)).inDays;

/// The all-day row over `days` (consecutive): each all-day or day-long event across the days it
/// covers, the earlier and then the longer ones in the upper rows.
({List<AllDayPlacement> placements, int rows}) layoutAllDay(List<engine.CalendarItem> events, List<DateTime> days) {
  final placed = <({engine.CalendarItem event, int first, int last})>[];
  for (final event in events) {
    if (!inAllDayRow(event)) continue;
    final span = eventSpan(event);
    // Ends at midnight: the day before is its last.
    final lastDay = span.end.isAfter(span.start) ? dayOf(span.end.subtract(const Duration(microseconds: 1))) : dayOf(span.start);
    final first = _daysBetween(days.first, dayOf(span.start)), last = _daysBetween(days.first, lastDay);
    if (last < 0 || first >= days.length) continue;
    placed.add((event: event, first: math.max(first, 0), last: math.min(last, days.length - 1)));
  }
  placed.sort((a, b) {
    if (a.first != b.first) return a.first.compareTo(b.first);
    if (a.last != b.last) return b.last.compareTo(a.last);
    return a.event.title.compareTo(b.event.title);
  });
  final rowEnds = <int>[];
  final out = <AllDayPlacement>[];
  for (final item in placed) {
    var row = rowEnds.indexWhere((end) => end < item.first);
    if (row < 0) {
      row = rowEnds.length;
      rowEnds.add(item.last);
    } else {
      rowEnds[row] = item.last;
    }
    out.add((event: item.event, first: item.first, last: item.last, row: row));
  }
  return (placements: out, rows: rowEnds.length);
}

/// How an event's card reads (MS-163): a white card for what is still ahead, hatched inside a
/// dashed border while the invitation waits for an answer (or got a maybe), flattened into the
/// tray once over or declined.
enum EventLook { upcoming, invited, past, declined }

EventLook eventLook(engine.CalendarItem event, DateTime now) {
  if (event.response == 'declined') return EventLook.declined;
  if (!eventSpan(event).end.isAfter(now)) return EventLook.past;
  if (event.response == 'needsAction' || event.response == 'tentative') return EventLook.invited;
  return EventLook.upcoming;
}

/// The account will be there: it accepted, organizes it, or it is its own event without guests.
bool _committed(engine.CalendarItem event) => switch (event.response) {
  null || 'accepted' || 'organizer' => true,
  _ => false,
};

/// Which of a day's placed events ([layoutDay]) clash: two the account is committed to whose
/// times overlap (not merely their drawn heights).
List<bool> dayConflicts(List<DayPlacement> placements) {
  final out = List.filled(placements.length, false);
  for (var i = 0; i < placements.length; i++) {
    final a = placements[i];
    if (a.columns < 2 || !_committed(a.event)) continue;
    final spanA = eventSpan(a.event);
    for (var j = i + 1; j < placements.length; j++) {
      final b = placements[j];
      if (b.columns < 2 || !_committed(b.event)) continue;
      final spanB = eventSpan(b.event);
      if (spanA.start.isBefore(spanB.end) && spanB.start.isBefore(spanA.end)) out[i] = out[j] = true;
    }
  }
  return out;
}

/// Hours a day is booked by its placed events, overlaps counted once and declined ones left out.
double bookedHours(List<DayPlacement> placements) {
  final spans = [for (final p in placements) if (p.event.response != 'declined') (p.top, p.bottom)]..sort((a, b) => a.$1.compareTo(b.$1));
  var minutes = 0, end = 0;
  for (final (top, bottom) in spans) {
    final from = math.max(top, end);
    if (bottom > from) minutes += bottom - from;
    end = math.max(end, bottom);
  }
  return minutes / 60;
}

/// Booked hours that fill a day's load bar.
const fullDayHours = 8;

/// How much of a card's content its stretch of time has room for: one line (title, start and
/// length), the title and time, or those over the call and the guests.
enum CardFit { line, standard, full }

CardFit cardFit(int minutes) => minutes < 45 ? CardFit.line : (minutes < 75 ? CardFit.standard : CardFit.full);

/// From `start` to `end`, the period once when both share it: 10:15 – 11:30 AM.
String timeRange(DateTime start, DateTime end) {
  final format = DateFormat.jm();
  final to = format.format(end);
  final twelveHour = format.pattern!.contains('a');
  if (twelveHour && (start.hour < 12) == (end.hour < 12)) return '${DateFormat('h:mm').format(start)} – $to';
  return '${format.format(start)} – $to';
}

/// An event's length, short: 15m, 1h, 1h 30m.
String shortDuration(Duration length) {
  final hours = length.inHours, minutes = length.inMinutes % 60;
  if (hours == 0) return '${minutes}m';
  return minutes == 0 ? '${hours}h' : '${hours}h ${minutes}m';
}

/// The video call's service, for its chip: Zoom, Teams, Webex, else Meet.
String meetingLabel(String url) {
  final host = Uri.tryParse(url)?.host.toLowerCase() ?? '';
  if (host.contains('zoom.')) return 'Zoom';
  if (host.contains('teams.')) return 'Teams';
  if (host.contains('webex.')) return 'Webex';
  return 'Meet';
}

/// The first day of the week `day` is in, as this locale starts weeks (Sunday in the US).
DateTime weekOf(DateTime day) {
  // intl counts from Monday = 0; DateTime from Monday = 1.
  final first = DateFormat.E().dateSymbols.FIRSTDAYOFWEEK + 1;
  return DateTime(day.year, day.month, day.day - (day.weekday - first) % 7);
}

/// The seven days from `start`.
List<DateTime> weekDays(DateTime start) => [for (var i = 0; i < 7; i++) DateTime(start.year, start.month, start.day + i)];

/// The week from `start`: November 8 – 14, 2026.
String weekTitle(DateTime start) {
  final end = DateTime(start.year, start.month, start.day + 6);
  if (start.year != end.year) return '${DateFormat('MMMM d, y').format(start)} – ${DateFormat('MMMM d, y').format(end)}';
  if (start.month != end.month) return '${DateFormat('MMMM d').format(start)} – ${DateFormat('MMMM d, y').format(end)}';
  return '${DateFormat('MMMM d').format(start)} – ${end.day}, ${end.year}';
}

/// The day view's day: Sunday, November 8, 2026.
String dayTitle(DateTime day) => DateFormat('EEEE, MMMM d, y').format(day);

/// What the engine fetched, for days shown outside it: Only Nov 1 – Jan 7 is synced.
String syncedNote(({DateTime from, DateTime until}) synced) {
  final last = DateTime(synced.until.year, synced.until.month, synced.until.day - 1);
  return 'Only ${DateFormat('MMM d').format(synced.from)} – ${DateFormat('MMM d').format(last)} is synced';
}

/// The calendar's views and paging, under its title: Agenda, Day and Week, then the previous
/// page, Today and the next page.
class CalendarViewBar extends StatelessWidget {
  const CalendarViewBar({super.key, required this.calendar});

  final CalendarState calendar;

  @override
  Widget build(BuildContext context) {
    final paged = calendar.view != CalendarView.agenda;
    final unit = calendar.view == CalendarView.week ? 'week' : 'day';
    return Row(children: [
      _ViewSwitch(view: calendar.view, onChanged: calendar.setView),
      const Spacer(),
      GlassCapsule(
        child: Row(mainAxisSize: MainAxisSize.min, children: [
          if (paged)
            ToolButton(tooltip: 'Previous $unit (←)', icon: CupertinoIcons.chevron_left, onPressed: () => calendar.page(-1)),
          ToolButton(tooltip: 'Today (T)', icon: CupertinoIcons.calendar_today, onPressed: calendar.today),
          if (paged) ToolButton(tooltip: 'Next $unit (→)', icon: CupertinoIcons.chevron_right, onPressed: () => calendar.page(1)),
        ]),
      ),
    ]);
  }
}

/// Agenda, Day and Week side by side, the one in view raised.
class _ViewSwitch extends StatelessWidget {
  const _ViewSwitch({required this.view, required this.onChanged});

  final CalendarView view;
  final ValueChanged<CalendarView> onChanged;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final light = colors.brightness == Brightness.light;
    Widget segment(CalendarView value, String label, String key) {
      final selected = value == view;
      return MacosTooltip(
        message: '$label ($key)',
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => onChanged(value),
          child: AnimatedContainer(
            duration: Motion.quick,
            padding: const EdgeInsets.symmetric(horizontal: 12),
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: selected ? (light ? MacosColors.white : colors.strongFill) : MacosColors.transparent,
              borderRadius: BorderRadius.circular(6),
              boxShadow: selected && light
                  ? [BoxShadow(color: MacosColors.black.withValues(alpha: 0.12), blurRadius: 2, offset: const Offset(0, 1))]
                  : null,
            ),
            child: Text(
              label,
              style: context.text.callout.copyWith(
                fontWeight: selected ? FontWeight.w600 : FontWeight.w500,
                color: selected ? colors.label : colors.secondaryLabel,
              ),
            ),
          ),
        ),
      );
    }

    return Container(
      height: 28,
      padding: const EdgeInsets.all(2),
      decoration: BoxDecoration(color: colors.strongFill.withValues(alpha: light ? 0.7 : 0.5), borderRadius: BorderRadius.circular(8)),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        segment(CalendarView.agenda, 'Agenda', 'A'),
        segment(CalendarView.day, 'Day', 'D'),
        segment(CalendarView.week, 'Week', 'W'),
      ]),
    );
  }
}

/// Days side by side on a 24-hour timeline, after MS-163: each day's date (today's raised on a
/// card, the others flat over a bar of how booked they are), the all-day row, then the hours with
/// the timed events as cards placed by [layoutDay], today's column lit, and the current time as a
/// line with its time in the gutter. Days the engine does not fetch are shaded and named over the
/// dates. The layout is worked out only when the events or days change; scrolling moves the
/// timeline's layer and the time line repaints alone, once a minute (the cards again only when
/// one of them ends).
class CalendarGrid extends StatefulWidget {
  const CalendarGrid({
    super.key,
    required this.days,
    required this.events,
    required this.colorOf,
    this.synced,
    this.selected,
    this.onOpen,
    this.onDay,
    this.onCreate,
    this.toNow,
  });

  /// Consecutive days, from the left.
  final List<DateTime> days;
  final List<engine.CalendarItem> events;
  final Color Function(engine.CalendarItem event) colorOf;
  final ({DateTime from, DateTime until})? synced;
  final EventKey? selected;
  final ValueChanged<engine.CalendarItem>? onOpen;

  /// A day's date was clicked.
  final ValueChanged<DateTime>? onDay;

  /// An empty stretch of time was clicked: a new event from there (to the quarter hour).
  final ValueChanged<DateTime>? onCreate;

  /// Scrolls the timeline to now when it fires.
  final Listenable? toNow;

  static const hourHeight = 48.0;
  static const gutter = 52.0;
  static const headerHeight = 44.0;
  static const allDayHeight = 22.0;
  static const allDayRows = 3;

  /// Room over the first hour line and under the last, for their labels.
  static const inset = 8.0;

  static double get timelineHeight => 24 * hourHeight + 2 * inset;

  @override
  State<CalendarGrid> createState() => _CalendarGridState();
}

class _CalendarGridState extends State<CalendarGrid> {
  ScrollController? _scroll;
  final _clock = ValueNotifier<DateTime>(DateTime.now());
  Timer? _tick;
  late List<List<DayPlacement>> _timed;
  late List<List<bool>> _conflicts;
  late List<double> _booked;
  late ({List<AllDayPlacement> placements, int rows}) _allDay;

  @override
  void initState() {
    super.initState();
    _layout();
    _scheduleTick();
    widget.toNow?.addListener(_onToNow);
  }

  @override
  void didUpdateWidget(CalendarGrid old) {
    super.didUpdateWidget(old);
    if (!identical(old.events, widget.events) || old.days.first != widget.days.first || old.days.length != widget.days.length) {
      _layout();
    }
    if (old.toNow != widget.toNow) {
      old.toNow?.removeListener(_onToNow);
      widget.toNow?.addListener(_onToNow);
    }
  }

  @override
  void dispose() {
    widget.toNow?.removeListener(_onToNow);
    _tick?.cancel();
    _clock.dispose();
    _scroll?.dispose();
    super.dispose();
  }

  void _layout() {
    _timed = [for (final day in widget.days) layoutDay(widget.events, day)];
    _conflicts = [for (final placements in _timed) dayConflicts(placements)];
    _booked = [for (final placements in _timed) bookedHours(placements)];
    _allDay = layoutAllDay(widget.events, widget.days);
  }

  /// Whether an event ended in `(from, to]`: its card flattens into the tray.
  bool _endedBetween(DateTime from, DateTime to) => widget.events.any((event) {
    final end = eventSpan(event).end;
    return end.isAfter(from) && !end.isAfter(to);
  });

  /// Whether an all-day row event goes on past the last day shown.
  bool _continuesAfter(engine.CalendarItem event) {
    final span = eventSpan(event);
    final last = span.end.isAfter(span.start) ? dayOf(span.end.subtract(const Duration(microseconds: 1))) : dayOf(span.start);
    return last.isAfter(widget.days.last);
  }

  /// Moves the time line on each minute; a new day rebuilds for today's date, an event ending
  /// for its card.
  void _scheduleTick() {
    final now = DateTime.now();
    _tick = Timer(Duration(seconds: 60 - now.second, milliseconds: -now.millisecond + 5), () {
      final before = _clock.value, after = DateTime.now();
      _clock.value = after;
      if (!mounted) return;
      if (!sameDayOf(before, after) || _endedBetween(before, after)) setState(() {});
      _scheduleTick();
    });
  }

  static bool sameDayOf(DateTime a, DateTime b) => a.year == b.year && a.month == b.month && a.day == b.day;

  /// Where the timeline opens: an hour and a half before now, so what is on now and next shows.
  double _nowOffset(double viewport) {
    final now = DateTime.now();
    final target = (_clockMinutes(now) / 60 - 1.5) * CalendarGrid.hourHeight;
    return target.clamp(0.0, math.max(0.0, CalendarGrid.timelineHeight - viewport));
  }

  void _onToNow() {
    // Today moves the days first: scroll once they are laid out.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final scroll = _scroll;
      if (!mounted || scroll == null || !scroll.hasClients) return;
      scroll.animateTo(_nowOffset(scroll.position.viewportDimension), duration: Motion.medium, curve: Motion.curve);
    });
  }

  bool _fetched(DateTime day) {
    final synced = widget.synced;
    return synced == null || (!day.isBefore(synced.from) && day.isBefore(synced.until));
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final days = widget.days;
    final today = dayOf(_clock.value);
    final todayIndex = days.indexWhere((day) => sameDayOf(day, today));
    final unfetched = [for (final day in days) !_fetched(day)];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (unfetched.contains(true))
          Padding(
            padding: const EdgeInsets.fromLTRB(18, 0, 18, 6),
            child: Text(syncedNote(widget.synced!), style: context.text.footnote.copyWith(color: colors.secondaryLabel)),
          ),
        SizedBox(
          height: CalendarGrid.headerHeight,
          child: Row(children: [
            const SizedBox(width: CalendarGrid.gutter),
            for (var i = 0; i < days.length; i++)
              Expanded(
                child: CalendarDayHeader(day: days[i], today: i == todayIndex, booked: _booked[i], onTap: widget.onDay),
              ),
          ]),
        ),
        if (_allDay.rows > 0) _allDayRow(context, today),
        const Hairline(),
        // Keyed: the synced note and the all-day row come and go above it without starting the
        // timeline over (its scroll position, the benchmark's controller).
        Expanded(
          key: const ValueKey('timeline'),
          child: LayoutBuilder(builder: (context, constraints) {
            final scroll = _scroll ??= ScrollController(initialScrollOffset: _nowOffset(constraints.maxHeight));
            ScrollBench.attach(scroll, scenario: ScrollBench.calendarWeek);
            return SingleChildScrollView(
              controller: scroll,
              child: SizedBox(
                height: CalendarGrid.timelineHeight,
                child: RepaintBoundary(
                  child: Stack(children: [
                    Positioned.fill(
                      child: CustomPaint(
                        painter: _HoursPainter(
                          unfetched: unfetched,
                          today: todayIndex,
                          spotlight: colors.spotlight,
                          line: colors.separator,
                          label: context.text.caption1.copyWith(color: colors.secondaryLabel),
                          shade: colors.label.withValues(alpha: colors.brightness == Brightness.light ? 0.035 : 0.05),
                        ),
                      ),
                    ),
                    Positioned(
                      left: CalendarGrid.gutter,
                      right: 0,
                      top: CalendarGrid.inset,
                      height: 24 * CalendarGrid.hourHeight,
                      child: Row(
                        crossAxisAlignment: CrossAxisAlignment.stretch,
                        children: [
                          for (var i = 0; i < days.length; i++)
                            Expanded(
                              child: _DayColumn(
                                day: days[i],
                                placements: _timed[i],
                                conflicts: _conflicts[i],
                                colorOf: widget.colorOf,
                                selected: widget.selected,
                                now: _clock.value,
                                onOpen: widget.onOpen,
                                onCreate: widget.onCreate,
                              ),
                            ),
                        ],
                      ),
                    ),
                    Positioned.fill(
                      child: IgnorePointer(
                        child: CustomPaint(
                          painter: _NowPainter(
                            clock: _clock,
                            days: days,
                            color: colors.red,
                            mask: colors.tray,
                            label: context.text.caption1.copyWith(
                              color: MacosColors.white,
                              fontWeight: FontWeight.w600,
                              fontFeatures: const [FontFeature.tabularFigures()],
                            ),
                          ),
                        ),
                      ),
                    ),
                  ]),
                ),
              ),
            );
          }),
        ),
      ],
    );
  }

  Widget _allDayRow(BuildContext context, DateTime today) {
    final colors = context.colors;
    final rows = _allDay.rows;
    final shown = math.min(rows, CalendarGrid.allDayRows);
    final content = SizedBox(
      height: rows * CalendarGrid.allDayHeight + 4,
      child: LayoutBuilder(builder: (context, constraints) {
        final column = (constraints.maxWidth - CalendarGrid.gutter) / widget.days.length;
        return Stack(children: [
          for (final placed in _allDay.placements)
            Positioned(
              left: CalendarGrid.gutter + placed.first * column + 1,
              width: (placed.last - placed.first + 1) * column - 2,
              top: placed.row * CalendarGrid.allDayHeight + 2,
              height: CalendarGrid.allDayHeight - 2,
              child: _AllDayBar(
                event: placed.event,
                color: widget.colorOf(placed.event),
                selected: widget.selected == eventKey(placed.event),
                past: !eventSpan(placed.event).end.isAfter(_clock.value),
                before: _daysBetween(widget.days.first, dayOf(eventSpan(placed.event).start)) < 0,
                after: placed.last == widget.days.length - 1 && _continuesAfter(placed.event),
                onTap: widget.onOpen == null ? null : () => widget.onOpen!(placed.event),
              ),
            ),
        ]);
      }),
    );
    return SizedBox(
      height: shown * CalendarGrid.allDayHeight + 4,
      child: Stack(children: [
        Positioned(
          left: 0,
          width: CalendarGrid.gutter - 6,
          top: 4,
          child: Text('all-day', textAlign: TextAlign.right, style: context.text.caption1.copyWith(color: colors.secondaryLabel)),
        ),
        Positioned.fill(child: rows > shown ? SingleChildScrollView(child: content) : content),
      ]),
    );
  }
}

/// A day's weekday and date. Today's lifts off the tray as a small card in the accent color; the
/// other days lie flat over a bar as long as their booked hours ([fullDayHours] fill it).
class CalendarDayHeader extends StatelessWidget {
  const CalendarDayHeader({super.key, required this.day, required this.today, required this.booked, required this.onTap});

  final DateTime day;
  final bool today;
  final double booked;
  final ValueChanged<DateTime>? onTap;

  /// The load bar of a fully booked day.
  static const _barWidth = 24.0;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final load = today || booked <= 0 ? 0.0 : (booked / fullDayHours).clamp(0.15, 1.0);
    final date = Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          DateFormat.E().format(day),
          style: context.text.caption1.copyWith(color: today ? colors.accent : colors.secondaryLabel, fontWeight: FontWeight.w500),
        ),
        Text(
          '${day.day}',
          style: context.text.callout.copyWith(fontWeight: today ? FontWeight.w700 : FontWeight.w600, color: today ? colors.accent : colors.label),
        ),
        const SizedBox(height: 3),
        SizedBox(
          key: load > 0 ? const ValueKey('load') : null,
          width: _barWidth * load,
          height: CalendarMetrics.loadBar,
          child: load > 0
              ? DecoratedBox(
                  decoration: BoxDecoration(
                    color: colors.accent.withValues(alpha: CalendarMetrics.loadOpacity),
                    borderRadius: BorderRadius.circular(CalendarMetrics.loadBar / 2),
                  ),
                )
              : null,
        ),
      ],
    );
    return Hoverable(
      onTap: onTap == null ? null : () => onTap!(day),
      child: Center(
        child: !today
            ? date
            : Container(
                key: const ValueKey('today'),
                padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 2),
                decoration: BoxDecoration(
                  color: colors.raisedCard,
                  borderRadius: BorderRadius.circular(CalendarMetrics.cardRadius),
                  border: Border.all(color: colors.cardBorder, width: CalendarMetrics.cardBorder),
                  boxShadow: colors.cardShadow,
                ),
                child: date,
              ),
      ),
    );
  }
}

/// One day's timed events as cards in their columns, [CalendarMetrics.conflictGap] apart; a
/// click in between starts a new event there.
class _DayColumn extends StatelessWidget {
  const _DayColumn({
    required this.day,
    required this.placements,
    required this.conflicts,
    required this.colorOf,
    required this.selected,
    required this.now,
    required this.onOpen,
    required this.onCreate,
  });

  final DateTime day;
  final List<DayPlacement> placements;
  final List<bool> conflicts;
  final Color Function(engine.CalendarItem event) colorOf;
  final EventKey? selected;
  final DateTime now;
  final ValueChanged<engine.CalendarItem>? onOpen;
  final ValueChanged<DateTime>? onCreate;

  @override
  Widget build(BuildContext context) {
    const perMinute = CalendarGrid.hourHeight / 60, gap = CalendarMetrics.conflictGap;
    return LayoutBuilder(builder: (context, constraints) {
      // Half a gap from the day's line on the left, a whole one before the next day's.
      final width = constraints.maxWidth - 1.5 * gap;
      return Stack(children: [
        if (onCreate != null)
          Positioned.fill(
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTapUp: (details) {
                final minutes = (details.localPosition.dy / perMinute).floor() ~/ 15 * 15;
                onCreate!(DateTime(day.year, day.month, day.day, 0, minutes.clamp(0, _dayMinutes - 15)));
              },
            ),
          ),
        for (var i = 0; i < placements.length; i++)
          () {
            final placed = placements[i];
            final cardWidth = width * placed.span / placed.columns - (placed.column + placed.span < placed.columns ? gap : 0);
            final cardHeight = (placed.bottom - placed.top) * perMinute - gap / 2;
            return Positioned(
              left: gap / 2 + width * placed.column / placed.columns,
              width: cardWidth,
              top: placed.top * perMinute,
              height: cardHeight,
              child: _EventCard(
                event: placed.event,
                color: colorOf(placed.event),
                selected: selected == eventKey(placed.event),
                look: eventLook(placed.event, now),
                conflict: conflicts[i],
                fit: cardFit(placed.bottom - placed.top),
                width: cardWidth,
                height: cardHeight,
                onTap: onOpen == null ? null : () => onOpen!(placed.event),
              ),
            );
          }(),
      ]);
    });
  }
}

/// What the account answered, for the chip on a wide card.
String? _answerLabel(String? response) => switch (response) {
  'accepted' => 'Going',
  'tentative' => 'Maybe',
  'needsAction' => 'Invited',
  _ => null,
};

/// An event on the timeline as a card on the tray (MS-163, variant A): its calendar's color only
/// in a dot before the title, the call as a quiet chip and the guests as a cluster at its foot
/// when there is room ([CardFit]). Open, it lifts with an accent rim; clashing with another the
/// account is going to, it shows a red dot; waiting for an answer, it is hatched inside a dashed
/// border; over or declined, it lies flat on the tray, faded ([EventLook]).
class _EventCard extends StatelessWidget {
  const _EventCard({
    required this.event,
    required this.color,
    required this.selected,
    required this.look,
    required this.conflict,
    required this.fit,
    required this.width,
    required this.height,
    required this.onTap,
  });

  final engine.CalendarItem event;
  final Color color;
  final bool selected;
  final EventLook look;
  final bool conflict;
  final CardFit fit;
  final double width;
  final double height;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final flat = look == EventLook.past || look == EventLook.declined;
    final fade = switch (look) {
      EventLook.past => CalendarMetrics.pastOpacity,
      EventLook.declined => CalendarMetrics.declinedOpacity,
      _ => 1.0,
    };
    final ink = colors.label.withValues(alpha: fade);
    final title = context.text.callout.copyWith(
      fontWeight: FontWeight.w600,
      color: ink,
      decoration: look == EventLook.declined ? TextDecoration.lineThrough : null,
      decorationColor: ink,
    );
    final detail = context.text.subheadline.copyWith(
      color: colors.secondaryLabel.withValues(alpha: fade),
      fontFeatures: const [FontFeature.tabularFigures()],
    );
    final span = eventSpan(event);
    final meeting = flat ? null : event.meetingUrl;
    final dot = Container(
      key: const ValueKey('dot'),
      width: CalendarMetrics.dot,
      height: CalendarMetrics.dot,
      decoration: BoxDecoration(color: calendarDot(color, colors).withValues(alpha: fade), shape: BoxShape.circle),
    );
    final answer = fit == CardFit.line || width < 150 || flat ? null : _answerLabel(event.response);
    final trailing = [
      if (answer != null) _QuietChip(label: answer),
      if (fit == CardFit.line && meeting != null && width >= 90)
        MacosIcon(CupertinoIcons.video_camera, size: 13, color: colors.secondaryLabel),
      if (conflict && look == EventLook.upcoming)
        Container(
          key: const ValueKey('conflict'),
          width: CalendarMetrics.dot,
          height: CalendarMetrics.dot,
          decoration: BoxDecoration(color: colors.red, shape: BoxShape.circle),
        ),
    ];
    final trailingRow = [for (final widget in trailing) Padding(padding: const EdgeInsets.only(left: 4), child: widget)];

    final Widget content;
    if (width < 36) {
      // A sliver beside its neighbors: the dot alone says something is there.
      content = Padding(padding: const EdgeInsets.fromLTRB(5, 5, 0, 0), child: Align(alignment: Alignment.topLeft, child: dot));
    } else if (fit == CardFit.line) {
      content = Padding(
        padding: const EdgeInsets.symmetric(horizontal: 6),
        child: Row(children: [
          dot,
          const SizedBox(width: 5),
          Expanded(
            child: Text.rich(
              TextSpan(children: [
                TextSpan(text: event.title, style: title),
                TextSpan(text: '  ${DateFormat.jm().format(span.start)} · ${shortDuration(span.end.difference(span.start))}', style: detail),
              ]),
              maxLines: 1,
              overflow: TextOverflow.clip,
              softWrap: false,
            ),
          ),
          ...trailingRow,
        ]),
      );
    } else {
      final when = width < 100 ? DateFormat.jm().format(span.start) : timeRange(span.start, span.end);
      final location = event.location;
      final guests = flat ? const <engine.CalendarPerson>[] : GuestAvatars.guestsOf(event);
      final foot = fit == CardFit.full && width >= 50 && (meeting != null || guests.isNotEmpty);
      // As many title lines as fit over the time (and the foot), each word whole: a word wider
      // than the card keeps the title to one line, cut with an ellipsis, rather than broken.
      final longest = event.title.split(' ').fold('', (a, b) => b.length > a.length ? b : a);
      final measure = TextPainter(text: TextSpan(text: longest, style: title), textDirection: TextDirection.ltr, maxLines: 1)
        ..layout();
      final wordTooWide = measure.width > width - 22 - (trailing.isEmpty ? 0 : 10);
      final lineHeight = measure.preferredLineHeight;
      measure.dispose();
      final room = height - 7 - lineHeight * (detail.fontSize! / title.fontSize!) - (foot ? CalendarMetrics.avatar + 2 : 0);
      final titleLines = wordTooWide ? 1 : (room / lineHeight).floor().clamp(1, 3);
      final text = Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Flexible(
            child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Padding(padding: EdgeInsets.only(top: (title.fontSize! * 1.2 - CalendarMetrics.dot) / 2), child: dot),
              const SizedBox(width: 5),
              Expanded(
                child: Text(
                  event.title,
                  maxLines: titleLines,
                  softWrap: !wordTooWide,
                  overflow: TextOverflow.ellipsis,
                  style: title,
                ),
              ),
              ...trailingRow,
            ]),
          ),
          Flexible(
            child: Text(
              location == null ? when : '$when · $location',
              maxLines: 1,
              softWrap: false,
              overflow: TextOverflow.ellipsis,
              style: detail,
            ),
          ),
        ],
      );
      content = Padding(
        padding: const EdgeInsets.fromLTRB(6, 3, 5, 4),
        child: !foot
            ? text
            : Stack(children: [
                Positioned.fill(bottom: CalendarMetrics.avatar + 2, child: text),
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 0,
                  height: CalendarMetrics.avatar,
                  // Too narrow for both: the guests alone.
                  child: Row(children: [
                    if (meeting != null && (guests.isEmpty || width >= 70))
                      _QuietChip(label: width >= 110 ? meetingLabel(meeting) : null, icon: CupertinoIcons.video_camera),
                    const Spacer(),
                    if (guests.isNotEmpty)
                      GuestAvatars(
                        event: event,
                        max: width >= 130 ? 3 : (width >= 96 ? 2 : 1),
                        ring: selected ? colors.liftedCard.last : colors.card,
                      ),
                  ]),
                ),
              ]),
      );
    }

    const radius = BorderRadius.all(Radius.circular(CalendarMetrics.cardRadius));
    final Widget card;
    if (!selected && look == EventLook.invited) {
      card = CustomPaint(
        key: const ValueKey('invited'),
        painter: _InvitedPainter(
          face: colors.card,
          hatch: color.withValues(alpha: CalendarMetrics.hatchOpacity),
          border: calendarDot(color, colors),
        ),
        child: ClipRRect(borderRadius: radius, child: content),
      );
    } else {
      card = Container(
        clipBehavior: Clip.hardEdge,
        decoration: selected
            ? BoxDecoration(
                gradient: LinearGradient(begin: Alignment.topCenter, end: Alignment.bottomCenter, colors: colors.liftedCard),
                borderRadius: radius,
                border: Border.all(color: colors.accent),
                boxShadow: colors.liftedShadow,
              )
            : switch (look) {
                EventLook.past => BoxDecoration(
                  color: Color.lerp(colors.tray, colors.card, fade),
                  borderRadius: radius,
                  border: Border.all(color: colors.cardBorder, width: CalendarMetrics.cardBorder),
                ),
                EventLook.declined => BoxDecoration(color: Color.alphaBlend(colors.hover, colors.tray), borderRadius: radius),
                _ => BoxDecoration(
                  color: colors.card,
                  borderRadius: radius,
                  border: Border.all(color: colors.cardBorder, width: CalendarMetrics.cardBorder),
                  boxShadow: colors.cardShadow,
                ),
              },
        child: content,
      );
    }
    return GestureDetector(behavior: HitTestBehavior.opaque, onTap: onTap, child: card);
  }
}

/// A small neutral capsule on a card: the answer (Going), the call (a camera, and its service
/// when there is room).
class _QuietChip extends StatelessWidget {
  const _QuietChip({this.label, this.icon});

  final String? label;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Container(
      height: CalendarMetrics.avatar,
      padding: const EdgeInsets.symmetric(horizontal: 5),
      decoration: ShapeDecoration(color: colors.fill, shape: const StadiumBorder()),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        if (icon != null) MacosIcon(icon, size: 11, color: colors.secondaryLabel),
        if (icon != null && label != null) const SizedBox(width: 3),
        if (label != null)
          Text(label!, maxLines: 1, style: context.text.caption1.copyWith(color: colors.secondaryLabel, fontWeight: FontWeight.w500)),
      ]),
    );
  }
}

/// An unanswered invitation's card: [face] hatched at 45° in [hatch] inside a dashed [border].
class _InvitedPainter extends CustomPainter {
  _InvitedPainter({required this.face, required this.hatch, required this.border});

  final Color face;
  final Color hatch;
  final Color border;

  @override
  void paint(Canvas canvas, Size size) {
    final card = RRect.fromRectAndRadius(Offset.zero & size, const Radius.circular(CalendarMetrics.cardRadius));
    canvas.drawRRect(card, Paint()..color = face);
    canvas.save();
    canvas.clipRRect(card);
    final lines = Paint()
      ..color = hatch
      ..strokeWidth = 1;
    for (var x = -size.height; x < size.width; x += CalendarMetrics.hatchSpacing) {
      canvas.drawLine(Offset(x, size.height), Offset(x + size.height, 0), lines);
    }
    canvas.restore();
    final stroke = Paint()
      ..color = border
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1;
    for (final metric in (Path()..addRRect(card.deflate(0.5))).computeMetrics()) {
      for (var at = 0.0; at < metric.length; at += 2 * CalendarMetrics.dash) {
        canvas.drawPath(metric.extractPath(at, at + CalendarMetrics.dash), stroke);
      }
    }
  }

  @override
  bool shouldRepaint(_InvitedPainter old) => old.face != face || old.hatch != hatch || old.border != border;
}

/// An event in the all-day row: a bar tinted with its calendar's color, raised with an accent rim
/// when open, flattened when over; ‹ and › where it goes on before or after the days shown.
class _AllDayBar extends StatelessWidget {
  const _AllDayBar({
    required this.event,
    required this.color,
    required this.selected,
    required this.past,
    required this.before,
    required this.after,
    required this.onTap,
  });

  final engine.CalendarItem event;
  final Color color;
  final bool selected;
  final bool past;
  final bool before;
  final bool after;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final fade = past ? CalendarMetrics.pastOpacity : 1.0;
    final style = context.text.callout.copyWith(fontWeight: FontWeight.w600, color: colors.label.withValues(alpha: fade));
    const radius = BorderRadius.all(Radius.circular(CalendarMetrics.cardRadius));
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: Container(
        clipBehavior: Clip.hardEdge,
        padding: const EdgeInsets.symmetric(horizontal: 6),
        alignment: Alignment.centerLeft,
        decoration: selected
            ? BoxDecoration(
                gradient: LinearGradient(begin: Alignment.topCenter, end: Alignment.bottomCenter, colors: colors.liftedCard),
                borderRadius: radius,
                border: Border.all(color: colors.accent),
                boxShadow: colors.liftedShadow,
              )
            : BoxDecoration(
                color: Color.alphaBlend(color.withValues(alpha: CalendarMetrics.allDayTint * fade), past ? colors.tray : colors.card),
                borderRadius: radius,
              ),
        child: Row(children: [
          if (before) Text('‹ ', style: style),
          Expanded(child: Text(event.title, maxLines: 1, softWrap: false, overflow: TextOverflow.ellipsis, style: style)),
          if (after) Text(' ›', style: style),
        ]),
      ),
    );
  }
}

/// One guest's monogram: their initial on their [guestColor], ringed in [ring] (the card under
/// it) when given.
class GuestMonogram extends StatelessWidget {
  const GuestMonogram({super.key, required this.person, this.size = CalendarMetrics.avatar, this.ring});

  final engine.CalendarPerson person;
  final double size;
  final Color? ring;

  @override
  Widget build(BuildContext context) {
    final color = guestColor(person.email ?? person.name ?? '');
    final ring = this.ring;
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        gradient: LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [Color.lerp(color, MacosColors.white, 0.18)!, color],
        ),
        border: ring == null ? null : Border.all(color: ring, width: CalendarMetrics.avatarRing, strokeAlign: BorderSide.strokeAlignOutside),
      ),
      child: Text(
        Address(name: person.name, email: person.email ?? '').initials,
        style: TextStyle(color: MacosColors.white, fontSize: size * 0.45, fontWeight: FontWeight.w600, height: 1),
      ),
    );
  }
}

/// An event's guests, the account itself left out, as overlapping [GuestMonogram]s: the first
/// [max], then +N for the rest.
class GuestAvatars extends StatelessWidget {
  const GuestAvatars({super.key, required this.event, this.max = 3, this.ring});

  final engine.CalendarItem event;
  final int max;

  /// The surface under the cluster, which rings each monogram; the resting card's by default.
  final Color? ring;

  /// The guests to show: everyone invited but the account.
  static List<engine.CalendarPerson> guestsOf(engine.CalendarItem event) {
    final account = event.account.toLowerCase();
    return [for (final person in event.attendees) if (person.email?.toLowerCase() != account) person];
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final guests = guestsOf(event);
    if (guests.isEmpty) return const SizedBox.shrink();
    final ring = this.ring ?? colors.card;
    const size = CalendarMetrics.avatar, step = size * 0.7;
    final shown = guests.length > max ? max : guests.length;
    final rest = guests.length - shown;
    final count = shown + (rest > 0 ? 1 : 0);
    return SizedBox(
      width: step * (count - 1) + size,
      height: size,
      child: Stack(clipBehavior: Clip.none, children: [
        for (var i = 0; i < shown; i++) Positioned(left: i * step, top: 0, child: GuestMonogram(person: guests[i], ring: ring)),
        if (rest > 0)
          Positioned(
            left: shown * step,
            top: 0,
            child: Container(
              width: size,
              height: size,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: colors.strongFill,
                shape: BoxShape.circle,
                border: Border.all(color: ring, width: CalendarMetrics.avatarRing, strokeAlign: BorderSide.strokeAlignOutside),
              ),
              child: Text(
                '+$rest',
                style: TextStyle(color: colors.secondaryLabel, fontSize: size * 0.4, fontWeight: FontWeight.w600, height: 1),
              ),
            ),
          ),
      ]),
    );
  }
}

/// The timeline's hour lines and their labels, the lines between days, the light over [today]'s
/// column and the shade over days the engine does not fetch.
class _HoursPainter extends CustomPainter {
  _HoursPainter({
    required this.unfetched,
    required this.today,
    required this.spotlight,
    required this.line,
    required this.label,
    required this.shade,
  });

  final List<bool> unfetched;

  /// Today's column, or -1 when today isn't shown.
  final int today;
  final Color spotlight;
  final Color line;
  final TextStyle label;
  final Color shade;

  @override
  void paint(Canvas canvas, Size size) {
    const gutter = CalendarGrid.gutter, inset = CalendarGrid.inset, hour = CalendarGrid.hourHeight;
    final column = (size.width - gutter) / unfetched.length;
    if (today >= 0) canvas.drawRect(Rect.fromLTWH(gutter + today * column, 0, column, size.height), Paint()..color = spotlight);
    final shadePaint = Paint()..color = shade;
    for (var i = 0; i < unfetched.length; i++) {
      if (unfetched[i]) canvas.drawRect(Rect.fromLTWH(gutter + i * column, 0, column, size.height), shadePaint);
    }
    final paint = Paint()
      ..color = line
      ..strokeWidth = 1;
    final format = DateFormat.j();
    for (var h = 0; h <= 24; h++) {
      final y = inset + h * hour;
      canvas.drawLine(Offset(gutter - 4, y), Offset(size.width, y), paint);
      if (h == 0 || h == 24) continue;
      final text = TextPainter(
        text: TextSpan(text: format.format(DateTime(2000, 1, 1, h)), style: label),
        textDirection: TextDirection.ltr,
      )..layout();
      text.paint(canvas, Offset(gutter - 8 - text.width, y - text.height / 2));
      text.dispose();
    }
    for (var i = 0; i < unfetched.length; i++) {
      final x = gutter + i * column;
      canvas.drawLine(Offset(x, 0), Offset(x, size.height), paint);
    }
  }

  @override
  bool shouldRepaint(_HoursPainter old) =>
      !listEquals(old.unfetched, unfetched) ||
      old.today != today ||
      old.spotlight != spotlight ||
      old.line != line ||
      old.label != label ||
      old.shade != shade;
}

/// The current time in the timeline's gutter: 10:08, or 22:08 where the clock runs to 24 hours.
String gutterTime(DateTime now) => DateFormat(DateFormat.jm().pattern!.contains('a') ? 'h:mm' : 'H:mm').format(now);

/// The current time: a pill with the time in the gutter (over the hour label it would collide
/// with), a line with a dot across today's column and a dashed one across the other days;
/// repaints on its own as [clock] moves.
class _NowPainter extends CustomPainter {
  _NowPainter({required this.clock, required this.days, required this.color, required this.mask, required this.label})
    : super(repaint: clock);

  final ValueListenable<DateTime> clock;
  final List<DateTime> days;
  final Color color;

  /// The gutter's background, which covers the hour label under the pill.
  final Color mask;
  final TextStyle label;

  static const _pillHeight = 16.0;

  @override
  void paint(Canvas canvas, Size size) {
    final now = clock.value;
    final index = days.indexWhere((day) => day.year == now.year && day.month == now.month && day.day == now.day);
    if (index < 0) return;
    const gutter = CalendarGrid.gutter, inset = CalendarGrid.inset, hour = CalendarGrid.hourHeight;
    final column = (size.width - gutter) / days.length;
    final minutes = _clockMinutes(now);
    final y = inset + minutes / 60 * hour;
    final left = gutter + index * column;

    final trail = Paint()
      ..color = color.withValues(alpha: CalendarMetrics.nowTrailOpacity)
      ..strokeWidth = 1;
    for (var x = gutter; x < size.width; x += 2 * CalendarMetrics.dash) {
      if (x + CalendarMetrics.dash > left && x < left + column) continue;
      canvas.drawLine(Offset(x, y), Offset(math.min(x + CalendarMetrics.dash, size.width), y), trail);
    }
    final solid = Paint()
      ..color = color
      ..strokeWidth = 1.5;
    canvas.drawLine(Offset(left, y), Offset(left + column, y), solid);
    canvas.drawCircle(Offset(left, y), CalendarMetrics.dot / 2, solid);

    final hourY = inset + (minutes / 60).round() * hour;
    if ((hourY - y).abs() < _pillHeight) {
      canvas.drawRect(Rect.fromLTRB(0, hourY - _pillHeight / 2, gutter - 4, hourY + _pillHeight / 2), Paint()..color = mask);
    }
    final text = TextPainter(text: TextSpan(text: gutterTime(now), style: label), textDirection: TextDirection.ltr)..layout();
    const padding = 5.0;
    final pill = RRect.fromLTRBR(
      gutter - 3 - text.width - 2 * padding,
      y - _pillHeight / 2,
      gutter - 3,
      y + _pillHeight / 2,
      const Radius.circular(_pillHeight / 2),
    );
    canvas.drawRRect(pill, Paint()..color = color);
    text.paint(canvas, Offset(pill.left + padding, y - text.height / 2));
    text.dispose();
  }

  @override
  bool shouldRepaint(_NowPainter old) =>
      old.clock != clock || old.color != color || old.mask != mask || old.label != label || !listEquals(old.days, days);
}
