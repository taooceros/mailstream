import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/foundation.dart' show ValueListenable, listEquals;
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart' hide TextDirection;
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

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

/// Days side by side on a 24-hour timeline: each day's date (today's in red), the all-day row,
/// then the hours with the timed events placed by [layoutDay] and a line at the current time.
/// Days the engine does not fetch are shaded and named over the dates. The layout is worked out
/// only when the events or days change; scrolling moves the timeline's layer and the time line
/// repaints alone, once a minute.
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
    _allDay = layoutAllDay(widget.events, widget.days);
  }

  /// Moves the time line on each minute; a new day rebuilds for today's date.
  void _scheduleTick() {
    final now = DateTime.now();
    _tick = Timer(Duration(seconds: 60 - now.second, milliseconds: -now.millisecond + 5), () {
      final before = _clock.value, after = DateTime.now();
      _clock.value = after;
      if (!mounted) return;
      if (!sameDayOf(before, after)) setState(() {});
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
            for (final day in days) Expanded(child: _DayHeader(day: day, today: sameDayOf(day, today), onTap: widget.onDay)),
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
                        child: CustomPaint(painter: _NowPainter(clock: _clock, days: days, color: colors.red)),
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
              child: _EventBlock(
                event: placed.event,
                color: widget.colorOf(placed.event),
                selected: widget.selected == eventKey(placed.event),
                past: !eventSpan(placed.event).end.isAfter(_clock.value),
                compact: true,
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

/// A day's weekday and date; today's date on red.
class _DayHeader extends StatelessWidget {
  const _DayHeader({required this.day, required this.today, required this.onTap});

  final DateTime day;
  final bool today;
  final ValueChanged<DateTime>? onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Hoverable(
      onTap: onTap == null ? null : () => onTap!(day),
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          Text(
            DateFormat.E().format(day),
            style: context.text.caption1.copyWith(color: today ? colors.red : colors.secondaryLabel, fontWeight: FontWeight.w500),
          ),
          const SizedBox(height: 1),
          Container(
            width: 24,
            height: 22,
            alignment: Alignment.center,
            decoration: today ? BoxDecoration(color: colors.red, borderRadius: BorderRadius.circular(11)) : null,
            child: Text(
              '${day.day}',
              style: context.text.callout.copyWith(fontWeight: FontWeight.w600, color: today ? MacosColors.white : colors.label),
            ),
          ),
        ],
      ),
    );
  }
}

/// One day's timed events, placed in their columns; a click in between starts a new event there.
class _DayColumn extends StatelessWidget {
  const _DayColumn({
    required this.day,
    required this.placements,
    required this.colorOf,
    required this.selected,
    required this.now,
    required this.onOpen,
    required this.onCreate,
  });

  final DateTime day;
  final List<DayPlacement> placements;
  final Color Function(engine.CalendarItem event) colorOf;
  final EventKey? selected;
  final DateTime now;
  final ValueChanged<engine.CalendarItem>? onOpen;
  final ValueChanged<DateTime>? onCreate;

  @override
  Widget build(BuildContext context) {
    const perMinute = CalendarGrid.hourHeight / 60;
    return LayoutBuilder(builder: (context, constraints) {
      final width = constraints.maxWidth - 2;
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
        for (final placed in placements)
          Positioned(
            left: width * placed.column / placed.columns,
            width: width * placed.span / placed.columns,
            top: placed.top * perMinute,
            height: (placed.bottom - placed.top) * perMinute,
            child: _EventBlock(
              event: placed.event,
              color: colorOf(placed.event),
              selected: selected == eventKey(placed.event),
              past: !eventSpan(placed.event).end.isAfter(now),
              compact: (placed.bottom - placed.top) < 40,
              onTap: onOpen == null ? null : () => onOpen!(placed.event),
            ),
          ),
      ]);
    });
  }
}

/// An event on the timeline or in the all-day row: tinted with its calendar's color and a bar
/// of it at the leading edge, solid when it is open; past and declined ones faded.
class _EventBlock extends StatelessWidget {
  const _EventBlock({
    required this.event,
    required this.color,
    required this.selected,
    required this.past,
    required this.compact,
    required this.onTap,
  });

  final engine.CalendarItem event;
  final Color color;
  final bool selected;
  final bool past;

  /// One line: the title, and the start after it on the timeline.
  final bool compact;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final light = colors.brightness == Brightness.light;
    final declined = event.response == 'declined';
    final dim = past || declined;
    // Opaque, so overlapping events don't show through each other.
    final tint = Color.alphaBlend(color.withValues(alpha: (light ? 0.22 : 0.34) * (dim ? 0.6 : 1)), colors.tray);
    final ink = selected ? MacosColors.white : (dim ? colors.secondaryLabel : colors.label);
    final title = context.text.footnote.copyWith(
      fontWeight: FontWeight.w600,
      color: ink,
      decoration: declined ? TextDecoration.lineThrough : null,
    );
    final timed = !event.allDay && !inAllDayRow(event);
    final start = timed ? DateFormat.jm().format(eventSpan(event).start) : null;
    final Widget label;
    if (compact) {
      label = Text.rich(
        TextSpan(children: [
          TextSpan(text: event.title, style: title),
          if (start != null && timed) TextSpan(text: '  $start', style: title.copyWith(fontWeight: FontWeight.w400)),
        ]),
        maxLines: 1,
        overflow: TextOverflow.clip,
        softWrap: false,
      );
    } else {
      label = Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Flexible(child: Text(event.title, maxLines: 3, overflow: TextOverflow.ellipsis, style: title)),
          if (start != null) Text(start, maxLines: 1, overflow: TextOverflow.clip, softWrap: false, style: title.copyWith(fontWeight: FontWeight.w400)),
          if (event.location != null)
            Flexible(
              child: Text(event.location!, maxLines: 1, overflow: TextOverflow.ellipsis, style: title.copyWith(fontWeight: FontWeight.w400)),
            ),
        ],
      );
    }
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: onTap,
      child: Container(
        clipBehavior: Clip.hardEdge,
        decoration: BoxDecoration(
          color: selected ? color : tint,
          borderRadius: BorderRadius.circular(4),
          border: Border.all(color: colors.tray, width: 0.5),
        ),
        child: Row(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
          Container(width: 3, color: color.withValues(alpha: dim ? 0.5 : 1)),
          Expanded(
            child: Padding(
              padding: EdgeInsets.fromLTRB(4, compact ? 1 : 2, 3, 1),
              child: compact ? Align(alignment: Alignment.centerLeft, child: label) : label,
            ),
          ),
        ]),
      ),
    );
  }
}

/// The timeline's hour lines and their labels, the lines between days and the shade over days
/// the engine does not fetch.
class _HoursPainter extends CustomPainter {
  _HoursPainter({required this.unfetched, required this.line, required this.label, required this.shade});

  final List<bool> unfetched;
  final Color line;
  final TextStyle label;
  final Color shade;

  @override
  void paint(Canvas canvas, Size size) {
    const gutter = CalendarGrid.gutter, inset = CalendarGrid.inset, hour = CalendarGrid.hourHeight;
    final column = (size.width - gutter) / unfetched.length;
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
      !listEquals(old.unfetched, unfetched) || old.line != line || old.label != label || old.shade != shade;
}

/// The current time across today's column, a dot at its leading end; repaints on its own as
/// [clock] moves.
class _NowPainter extends CustomPainter {
  _NowPainter({required this.clock, required this.days, required this.color}) : super(repaint: clock);

  final ValueListenable<DateTime> clock;
  final List<DateTime> days;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final now = clock.value;
    final index = days.indexWhere((day) => day.year == now.year && day.month == now.month && day.day == now.day);
    if (index < 0) return;
    const gutter = CalendarGrid.gutter;
    final column = (size.width - gutter) / days.length;
    final y = CalendarGrid.inset + _clockMinutes(now) / 60 * CalendarGrid.hourHeight;
    final left = gutter + index * column;
    final paint = Paint()
      ..color = color
      ..strokeWidth = 1.5;
    canvas.drawLine(Offset(left, y), Offset(left + column, y), paint);
    canvas.drawCircle(Offset(left, y), 4, paint);
  }

  @override
  bool shouldRepaint(_NowPainter old) => old.clock != clock || old.color != color || !listEquals(old.days, days);
}
