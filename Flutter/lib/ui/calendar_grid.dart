import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/foundation.dart' show ValueListenable, listEquals;
import 'package:flutter/gestures.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart' hide TextDirection;
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart' show Address;
import '../bench.dart';
import '../state/calendar_state.dart';
import 'calendar.dart';
import 'common.dart';
import 'controls.dart';
import 'design.dart';
import 'glass.dart';
import 'slide_layer.dart';

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

/// Whether every word of `text` fits on a line `width` wide in `style`, so wrapping it never
/// breaks inside a word.
bool wordsFit(String text, TextStyle style, TextScaler scaler, double width) {
  final painter = TextPainter(textDirection: TextDirection.ltr, textScaler: scaler, maxLines: 1);
  try {
    for (final word in text.split(RegExp(r'\s+'))) {
      if (word.isEmpty) continue;
      painter
        ..text = TextSpan(text: word, style: style)
        ..layout();
      if (painter.width > width) return false;
    }
    return true;
  } finally {
    painter.dispose();
  }
}

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

/// Agenda, Day and Week side by side, equally wide, the one in view raised on a capsule that
/// slides under the next one picked.
class _ViewSwitch extends StatelessWidget {
  const _ViewSwitch({required this.view, required this.onChanged});

  final CalendarView view;
  final ValueChanged<CalendarView> onChanged;

  static const _views = [(CalendarView.agenda, 'Agenda', 'A'), (CalendarView.day, 'Day', 'D'), (CalendarView.week, 'Week', 'W')];

  @override
  Widget build(BuildContext context) => MailSegmented<CalendarView>(
    selected: view,
    onChanged: onChanged,
    segments: [for (final (value, label, key) in _views) MailSegment(value, label, tooltip: '$label ($key)')],
  );
}

/// Where the timeline is scrolled, kept by the calendar's pane for the grids that show it in turn:
/// switching between Day and Week keeps the same hours in view.
class TimelineOffset {
  double? pixels;
}

/// Days side by side on a 24-hour timeline, after MS-163: each day's date (today's raised on a
/// card, the others flat over a bar of how booked they are), the all-day row, then the hours with
/// the timed events as cards placed by [layoutDay], today's column lit, and the current time as a
/// line with its time in the gutter. Days the engine does not fetch are shaded and named over the
/// dates. The layout is worked out only when the events or days change; scrolling moves the
/// timeline's layer and the time line repaints alone, once a minute (the cards again only when
/// one of them ends).
///
/// The days page as one (MS-170): a two-finger swipe on the trackpad drags them 1:1, the gutter's
/// hours staying put, and past a third of the width or flicked they spring on to the next page
/// ([Motion.spring], as the reader does), else back; a new [days] one page on (← and →) slides in
/// over [Motion.page]. The pages either side are laid out at rest, each in its own layers, so
/// paging only moves layers.
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
    this.onPage,
    this.toNow,
    this.offset,
  });

  /// Consecutive days, from the left: one page.
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

  /// A swipe went on to the next page (1) or the one before (-1); the owner moves [days] by it.
  final ValueChanged<int>? onPage;

  /// Scrolls the timeline to now when it fires.
  final Listenable? toNow;

  /// Where the timeline opens and what it keeps as it scrolls; none: around now.
  final TimelineOffset? offset;

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

/// One page of the grid: its days laid out ([layoutDay], [layoutAllDay]) and, once built, its
/// widgets, which stay the same objects until what they show changes so paging rebuilds none.
class _Page {
  _Page(List<engine.CalendarItem> events, this.days) {
    layout(events);
  }

  final List<DateTime> days;
  late List<List<DayPlacement>> timed;
  late List<List<bool>> conflicts;
  late List<double> booked;
  late ({List<AllDayPlacement> placements, int rows}) allDay;
  Widget? header, allDayRow, timeline;

  void layout(List<engine.CalendarItem> events) {
    timed = [for (final day in days) layoutDay(events, day)];
    conflicts = [for (final placements in timed) dayConflicts(placements)];
    booked = [for (final placements in timed) bookedHours(placements)];
    allDay = layoutAllDay(events, days);
    forget();
  }

  void forget() => header = allDayRow = timeline = null;

  /// Rows the all-day row shows, the rest scrolling.
  int get shownRows => math.min(allDay.rows, CalendarGrid.allDayRows);
}

class _CalendarGridState extends State<CalendarGrid> with TickerProviderStateMixin {
  ScrollController? _scroll;
  final _clock = ValueNotifier<DateTime>(DateTime.now());
  Timer? _tick;

  /// The pages built, by first day: the one shown and, at rest, one either side; while paging,
  /// also the ones it left.
  final _pages = <DateTime, _Page>{};

  /// Where the pages are, in page widths: 0 the shown one in place, -0.5 halfway to the next.
  late final _pan = AnimationController.unbounded(vsync: this);
  final _swipe = HorizontalSwipe();
  late final _ready = SwipeReadiness(this);

  /// [_pan] when the swipe turned horizontal; none until then.
  double? _origin;

  /// The release velocity, in pages a second, a swipe paging on hands to the page's spring.
  double? _fling;

  /// Fingers are on the trackpad or the pages are moving: the pages either side are drawn.
  bool _moving = false;
  bool _fingers = false;
  bool _neighborsQueued = false;
  double _pageWidth = 1;

  int get _length => widget.days.length;
  DateTime get _first => widget.days.first;

  /// The first day of the page `index` pages from the one shown.
  DateTime _pageStart(int index) => DateTime(_first.year, _first.month, _first.day + index * _length);

  _Page _page(DateTime start) =>
      _pages[start] ??= _Page(widget.events, [for (var i = 0; i < _length; i++) DateTime(start.year, start.month, start.day + i)]);

  @override
  void initState() {
    super.initState();
    _page(_first);
    _ready.shown = true;
    _scheduleTick();
    _queueNeighbors();
    widget.toNow?.addListener(_onToNow);
    CalendarPageBench.attachGrid(paging: () => mounted && (_moving || _pan.isAnimating), area: _area);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // The theme may have changed: the cards' colors.
    _forget();
  }

  @override
  void didUpdateWidget(CalendarGrid old) {
    super.didUpdateWidget(old);
    if (!identical(old.events, widget.events)) {
      for (final page in _pages.values) {
        page.layout(widget.events);
      }
    } else if (old.selected != widget.selected ||
        old.synced != widget.synced ||
        (old.onOpen == null) != (widget.onOpen == null) ||
        (old.onDay == null) != (widget.onDay == null) ||
        (old.onCreate == null) != (widget.onCreate == null)) {
      _forget();
    }
    final moved = old.days.length == _length ? _daysBetween(old.days.first, _first) : null;
    final pages = moved != null && moved % _length == 0 ? moved ~/ _length : null;
    if (pages == null || pages.abs() > 1) {
      // Another view's days, or a jump (Today): the new page shows at once.
      _pan
        ..stop()
        ..value = 0;
      _fling = null;
      _pages.clear();
      _page(_first);
      _moving = _fingers;
      _queueNeighbors();
    } else if (pages != 0) {
      // One page on: the old one stays where it was and both move on together.
      _page(_first);
      _moving = true;
      _pan.value += pages;
      final fling = _fling;
      _fling = null;
      (fling != null
              ? _pan.animateWith(settleSwipe(_pan.value, 0, fling, pixels: _pageWidth))
              : _pan.animateTo(0, duration: Motion.page, curve: Motion.curve))
          .whenComplete(_settled);
    }
    if (old.toNow != widget.toNow) {
      old.toNow?.removeListener(_onToNow);
      widget.toNow?.addListener(_onToNow);
    }
    final selected = widget.selected;
    if (selected != null && selected != old.selected) WidgetsBinding.instance.addPostFrameCallback((_) => _reveal(selected));
  }

  @override
  void dispose() {
    widget.toNow?.removeListener(_onToNow);
    _tick?.cancel();
    _clock.dispose();
    _scroll?.dispose();
    _ready.dispose();
    _pan.dispose();
    super.dispose();
  }

  /// The pages' widgets show something else now (the open event, the time, the theme).
  void _forget() {
    for (final page in _pages.values) {
      page.forget();
    }
  }

  /// Lays out the pages either side of the one shown after the first frame in which nothing
  /// animates (no page sliding, no view fading in), and lets go of the others: a page's first
  /// layout never lands in the middle of a slide.
  void _queueNeighbors() {
    if (_neighborsQueued) return;
    _neighborsQueued = true;
    final scheduler = SchedulerBinding.instance;
    void check(Duration _) {
      if (!mounted) return;
      if (_moving || scheduler.transientCallbackCount > 0) {
        scheduler.addPostFrameCallback(check);
        return;
      }
      _neighborsQueued = false;
      setState(() {
        final keep = {for (var i = -1; i <= 1; i++) _pageStart(i)};
        _pages.removeWhere((start, _) => !keep.contains(start));
        keep.forEach(_page);
      });
    }

    scheduler
      ..addPostFrameCallback(check)
      ..ensureVisualUpdate();
  }

  /// The pages came to rest.
  void _settled() {
    if (!mounted || _fingers) return;
    if (!_pan.isAnimating) _pan.value = 0;
    if (_moving) setState(() => _moving = false);
    _queueNeighbors();
  }

  void _start(PointerPanZoomStartEvent event) {
    _swipe.start(event);
    _origin = null;
    _fingers = true;
    _ready.fingersDown();
    // Normally laid out already, at rest; else now, while the fingers have yet to move.
    if (!_moving || !_pages.containsKey(_pageStart(-1)) || !_pages.containsKey(_pageStart(1))) {
      setState(() {
        _moving = true;
        _page(_pageStart(-1));
        _page(_pageStart(1));
      });
    }
  }

  void _update(PointerPanZoomUpdateEvent event) {
    if (!_swipe.update(event)) return;
    if (_origin == null) {
      _ready.swipeTookOver();
      _pan.stop();
      _fling = null;
      _origin = _pan.value;
    }
    // One page either way: the next one's neighbor is laid out once it rests.
    _pan.value = (_origin! + event.pan.dx / _pageWidth).clamp(-1.0, 1.0);
  }

  void _end() {
    _ready.swipeTookOver();
    _fingers = false;
    final velocity = _swipe.end();
    final dragged = _origin != null;
    _origin = null;
    if (velocity == null || !dragged) {
      if (!_pan.isAnimating) _settled();
      return;
    }
    final width = _pageWidth, offset = _pan.value * width, onPage = widget.onPage;
    if (onPage != null && offset != 0 && HorizontalSwipe.dismisses(offset, velocity, width)) {
      // The owner moves the days on; the page's spring starts from here with this velocity.
      _fling = velocity / width;
      onPage(offset < 0 ? 1 : -1);
      SchedulerBinding.instance
        ..ensureVisualUpdate()
        ..addPostFrameCallback((_) {
          // The days did not move on: back to the page shown.
          if (mounted && _fling != null) _springBack(_fling!);
        });
    } else {
      _springBack(velocity / width);
    }
  }

  void _springBack(double velocity) {
    _fling = null;
    _pan.animateWith(settleSwipe(_pan.value, 0, velocity, pixels: _pageWidth)).whenComplete(_settled);
  }

  /// The grid on screen, for the paging benchmark.
  Rect _area() {
    final box = context.findRenderObject()! as RenderBox;
    return box.localToGlobal(Offset.zero) & box.size;
  }

  /// Whether an event ended in `(from, to]`: its card flattens into the tray.
  bool _endedBetween(DateTime from, DateTime to) => widget.events.any((event) {
    final end = eventSpan(event).end;
    return end.isAfter(from) && !end.isAfter(to);
  });

  /// Moves the time line on each minute; a new day rebuilds for today's date, an event ending
  /// for its card.
  void _scheduleTick() {
    final now = DateTime.now();
    _tick = Timer(Duration(seconds: 60 - now.second, milliseconds: -now.millisecond + 5), () {
      final before = _clock.value, after = DateTime.now();
      _clock.value = after;
      if (!mounted) return;
      if (!sameDayOf(before, after) || _endedBetween(before, after)) setState(_forget);
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

  void _keepOffset() => widget.offset?.pixels = _scroll!.offset;

  /// Brings the open event's card into view when it is off the timeline's visible hours (↑ and ↓
  /// open the next one): at once, as the event beside the grid changes at once.
  void _reveal(EventKey key) {
    final scroll = _scroll;
    final page = _pages[_first];
    if (!mounted || scroll == null || !scroll.hasClients || page == null) return;
    const perMinute = CalendarGrid.hourHeight / 60;
    for (final placements in page.timed) {
      for (final placed in placements) {
        if (eventKey(placed.event) != key) continue;
        final top = CalendarGrid.inset + placed.top * perMinute, bottom = CalendarGrid.inset + placed.bottom * perMinute;
        final position = scroll.position;
        final margin = CalendarGrid.hourHeight / 2;
        if (top < position.pixels) {
          scroll.jumpTo(math.max(position.minScrollExtent, top - margin));
        } else if (bottom > position.pixels + position.viewportDimension) {
          scroll.jumpTo(math.min(position.maxScrollExtent, math.max(top - margin, bottom + margin - position.viewportDimension)));
        }
        return;
      }
    }
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
    final current = _page(_first);
    final showsToday = days.any((day) => sameDayOf(day, today));
    // The pages built, each with how many pages from the one shown it is.
    final pages = [for (final page in _pages.values) (index: _daysBetween(_first, page.days.first) ~/ _length, page: page)];
    final rows = current.shownRows;
    return Listener(
      onPointerPanZoomStart: _start,
      onPointerPanZoomUpdate: _update,
      onPointerPanZoomEnd: (_) => _end(),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (days.any((day) => !_fetched(day)))
            Padding(
              padding: const EdgeInsets.fromLTRB(18, 0, 18, 6),
              child: Text(syncedNote(widget.synced!), style: context.text.footnote.copyWith(color: colors.secondaryLabel)),
            ),
          SizedBox(
            height: CalendarGrid.headerHeight,
            child: Row(children: [
              const SizedBox(width: CalendarGrid.gutter),
              Expanded(child: _strip(pages, (page) => page.header ??= _header(page, today))),
            ]),
          ),
          // Grows and shrinks with the page shown; each page keeps its own height inside.
          AnimatedContainer(
            duration: Motion.page,
            curve: Motion.curve,
            height: rows > 0 ? rows * CalendarGrid.allDayHeight + 4 : 0,
            child: ClipRect(
              child: Stack(children: [
                if (rows > 0)
                  Positioned(
                    left: 0,
                    width: CalendarGrid.gutter - 6,
                    top: 4,
                    child: Text('all-day', textAlign: TextAlign.right, style: context.text.caption1.copyWith(color: colors.secondaryLabel)),
                  ),
                Positioned.fill(
                  left: CalendarGrid.gutter,
                  child: _strip(pages, (page) => page.allDayRow ??= _allDayRow(page), heightOf: (page) => page.shownRows * CalendarGrid.allDayHeight + 4),
                ),
              ]),
            ),
          ),
          const Hairline(),
          // Keyed: the synced note and the all-day row come and go above it without starting the
          // timeline over (its scroll position, the benchmark's controller).
          Expanded(
            key: const ValueKey('timeline'),
            child: LayoutBuilder(builder: (context, constraints) {
              _pageWidth = math.max(1, constraints.maxWidth - CalendarGrid.gutter);
              final scroll = _scroll ??= ScrollController(
                initialScrollOffset: widget.offset?.pixels ?? _nowOffset(constraints.maxHeight),
              )..addListener(_keepOffset);
              ScrollBench.attach(scroll, scenario: ScrollBench.calendarWeek);
              return SingleChildScrollView(
                controller: scroll,
                child: SizedBox(
                  height: CalendarGrid.timelineHeight,
                  child: Stack(children: [
                    // The hours stay put as the days page.
                    Positioned(
                      left: 0,
                      width: CalendarGrid.gutter,
                      top: 0,
                      bottom: 0,
                      child: RepaintBoundary(
                        child: CustomPaint(
                          painter: _GutterPainter(
                            clock: _clock,
                            now: showsToday,
                            line: colors.separator,
                            label: context.text.caption1.copyWith(color: colors.secondaryLabel),
                            color: colors.red,
                            mask: colors.tray,
                            nowLabel: context.text.caption1.copyWith(
                              color: MacosColors.white,
                              fontWeight: FontWeight.w600,
                              fontFeatures: const [FontFeature.tabularFigures()],
                            ),
                          ),
                        ),
                      ),
                    ),
                    Positioned(
                      left: CalendarGrid.gutter,
                      right: 0,
                      top: 0,
                      bottom: 0,
                      child: _strip(pages, (page) => page.timeline ??= _timeline(page, today), clipper: const _NowDotClip()),
                    ),
                  ]),
                ),
              );
            }),
          ),
        ],
      ),
    );
  }

  /// The pages side by side in one band (the dates, the all-day row or the timeline), each
  /// [heightOf] tall from the top (else the band's height), the ones beside the shown page drawn
  /// only while paging.
  Widget _strip(
    List<({int index, _Page page})> pages,
    Widget Function(_Page page) content, {
    double Function(_Page page)? heightOf,
    CustomClipper<Rect>? clipper,
  }) => ClipRect(
    clipper: clipper,
    child: Stack(
      fit: StackFit.expand,
      children: [
        for (final (:index, :page) in pages)
          Positioned(
            key: ValueKey(page.days.first),
            left: 0,
            right: 0,
            top: 0,
            height: heightOf?.call(page),
            bottom: heightOf == null ? 0 : null,
            child: _PageSlide(
              pan: _pan,
              index: index,
              child: Offstage(offstage: index != 0 && !_moving, child: content(page)),
            ),
          ),
      ],
    ),
  );

  Widget _header(_Page page, DateTime today) => RepaintBoundary(
    child: Row(children: [
      for (var i = 0; i < page.days.length; i++)
        Expanded(
          child: CalendarDayHeader(day: page.days[i], today: sameDayOf(page.days[i], today), booked: page.booked[i], onTap: widget.onDay),
        ),
    ]),
  );

  Widget _timeline(_Page page, DateTime today) {
    final colors = context.colors;
    final days = page.days;
    return RepaintBoundary(
      child: Stack(children: [
        Positioned.fill(
          child: CustomPaint(
            painter: _DaysPainter(
              unfetched: [for (final day in days) !_fetched(day)],
              today: days.indexWhere((day) => sameDayOf(day, today)),
              spotlight: colors.spotlight,
              line: colors.separator,
              shade: colors.label.withValues(alpha: colors.brightness == Brightness.light ? 0.035 : 0.05),
            ),
          ),
        ),
        Positioned(
          left: 0,
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
                    placements: page.timed[i],
                    conflicts: page.conflicts[i],
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
            child: RepaintBoundary(child: CustomPaint(painter: _NowLinePainter(clock: _clock, days: days, color: colors.red))),
          ),
        ),
      ]),
    );
  }

  Widget _allDayRow(_Page page) {
    final rows = page.allDay.rows;
    final content = SizedBox(
      height: rows * CalendarGrid.allDayHeight + 4,
      child: LayoutBuilder(builder: (context, constraints) {
        final column = constraints.maxWidth / page.days.length;
        return Stack(children: [
          for (final placed in page.allDay.placements)
            Positioned(
              left: placed.first * column + 1,
              width: (placed.last - placed.first + 1) * column - 2,
              top: placed.row * CalendarGrid.allDayHeight + 2,
              height: CalendarGrid.allDayHeight - 2,
              child: _AllDayBar(
                event: placed.event,
                color: widget.colorOf(placed.event),
                selected: widget.selected == eventKey(placed.event),
                past: !eventSpan(placed.event).end.isAfter(_clock.value),
                before: _daysBetween(page.days.first, dayOf(eventSpan(placed.event).start)) < 0,
                after: placed.last == page.days.length - 1 && _continuesAfter(placed.event, page.days.last),
                onTap: widget.onOpen == null ? null : () => widget.onOpen!(placed.event),
              ),
            ),
        ]);
      }),
    );
    return RepaintBoundary(child: rows > page.shownRows ? SingleChildScrollView(child: content) : content);
  }

  /// Whether an all-day row event goes on past `last`, the page's last day.
  static bool _continuesAfter(engine.CalendarItem event, DateTime last) {
    final span = eventSpan(event);
    final end = span.end.isAfter(span.start) ? dayOf(span.end.subtract(const Duration(microseconds: 1))) : dayOf(span.start);
    return end.isAfter(last);
  }
}

/// A page of the grid [index] pages from the one shown, moved by [pan] (page widths): a
/// translation of its layers alone, never a new layout.
class _PageSlide extends AnimatedWidget {
  const _PageSlide({required Animation<double> pan, required this.index, required this.child}) : super(listenable: pan);

  final int index;
  final Widget child;

  @override
  Widget build(BuildContext context) =>
      FractionalTranslation(translation: Offset(index + (listenable as Animation<double>).value, 0), child: child);
}

/// The timeline's pages, clipped at the gutter but for the current time's dot on the first day's
/// line.
class _NowDotClip extends CustomClipper<Rect> {
  const _NowDotClip();

  @override
  Rect getClip(Size size) => Rect.fromLTRB(-MarkMetrics.dot / 2, 0, size.width, size.height);

  @override
  bool shouldReclip(_NowDotClip old) => false;
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
    final dot = MarkDot(calendarDot(color, colors).withValues(alpha: fade), key: const ValueKey('dot'));
    final answer = fit == CardFit.line || width < 150 || flat ? null : _answerLabel(event.response);
    final trailing = [
      if (answer != null) Tag(label: answer, compact: true),
      if (fit == CardFit.line && meeting != null && width >= 90)
        MacosIcon(CupertinoIcons.video_camera, size: 13, color: colors.secondaryLabel),
      if (conflict && look == EventLook.upcoming) MarkDot(colors.red, key: const ValueKey('conflict')),
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
              overflow: TextOverflow.ellipsis,
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
      // As many title lines as fit over the time (and the foot).
      final scaler = MediaQuery.textScalerOf(context);
      final shown = DefaultTextStyle.of(context).style.merge(title);
      final template = TextPainter(text: TextSpan(style: shown), textDirection: TextDirection.ltr, textScaler: scaler);
      final lineHeight = template.preferredLineHeight;
      template.dispose();
      final room = height - 7 - lineHeight * (detail.fontSize! / title.fontSize!) - (foot ? CalendarMetrics.avatar + 2 : 0);
      final titleLines = (room / lineHeight).floor().clamp(1, 3);
      final text = Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Flexible(
            child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Padding(padding: EdgeInsets.only(top: (title.fontSize! * 1.2 - MarkMetrics.dot) / 2), child: dot),
              const SizedBox(width: 5),
              Expanded(
                // Each word whole: measured against the width the title really gets (after the
                // card's border and what trails it), a word wider than that keeps the title to
                // one line cut with an ellipsis, rather than broken inside the word (MS-168).
                child: LayoutBuilder(builder: (context, constraints) {
                  final whole = wordsFit(event.title, shown, scaler, constraints.maxWidth);
                  return Text(
                    event.title,
                    maxLines: whole ? titleLines : 1,
                    softWrap: whole,
                    overflow: TextOverflow.ellipsis,
                    style: title,
                  );
                }),
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
                      Flexible(child: Tag(label: width >= 110 ? meetingLabel(meeting) : null, icon: CupertinoIcons.video_camera, compact: true)),
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

/// A page's days on the timeline: the hour lines, the lines between days (the first day's left
/// edge too), the light over [today]'s column and the shade over days the engine does not fetch.
class _DaysPainter extends CustomPainter {
  _DaysPainter({required this.unfetched, required this.today, required this.spotlight, required this.line, required this.shade});

  final List<bool> unfetched;

  /// Today's column, or -1 when today isn't on the page.
  final int today;
  final Color spotlight;
  final Color line;
  final Color shade;

  @override
  void paint(Canvas canvas, Size size) {
    const inset = CalendarGrid.inset, hour = CalendarGrid.hourHeight;
    final column = size.width / unfetched.length;
    if (today >= 0) canvas.drawRect(Rect.fromLTWH(today * column, 0, column, size.height), Paint()..color = spotlight);
    final shadePaint = Paint()..color = shade;
    for (var i = 0; i < unfetched.length; i++) {
      if (unfetched[i]) canvas.drawRect(Rect.fromLTWH(i * column, 0, column, size.height), shadePaint);
    }
    final paint = Paint()
      ..color = line
      ..strokeWidth = 1;
    for (var h = 0; h <= 24; h++) {
      final y = inset + h * hour;
      canvas.drawLine(Offset(0, y), Offset(size.width, y), paint);
    }
    for (var i = 0; i < unfetched.length; i++) {
      final x = i * column;
      canvas.drawLine(Offset(x, 0), Offset(x, size.height), paint);
    }
  }

  @override
  bool shouldRepaint(_DaysPainter old) =>
      !listEquals(old.unfetched, unfetched) || old.today != today || old.spotlight != spotlight || old.line != line || old.shade != shade;
}

/// The current time in the timeline's gutter: 10:08, or 22:08 where the clock runs to 24 hours.
String gutterTime(DateTime now) => DateFormat(DateFormat.jm().pattern!.contains('a') ? 'h:mm' : 'H:mm').format(now);

const _nowPillHeight = 16.0;

/// The gutter, which stays put as the days page: the hours' labels and the ends of their lines,
/// and, when the page shown has today ([now]), the current time as a pill (over the hour label
/// it would collide with). Repaints on its own as [clock] moves.
class _GutterPainter extends CustomPainter {
  _GutterPainter({
    required this.clock,
    required this.now,
    required this.line,
    required this.label,
    required this.color,
    required this.mask,
    required this.nowLabel,
  }) : super(repaint: clock);

  final ValueListenable<DateTime> clock;
  final bool now;
  final Color line;
  final TextStyle label;
  final Color color;

  /// The gutter's background, which covers the hour label under the pill.
  final Color mask;
  final TextStyle nowLabel;

  @override
  void paint(Canvas canvas, Size size) {
    const gutter = CalendarGrid.gutter, inset = CalendarGrid.inset, hour = CalendarGrid.hourHeight;
    final paint = Paint()
      ..color = line
      ..strokeWidth = 1;
    final format = DateFormat.j();
    for (var h = 0; h <= 24; h++) {
      final y = inset + h * hour;
      canvas.drawLine(Offset(gutter - 4, y), Offset(gutter, y), paint);
      if (h == 0 || h == 24) continue;
      final text = TextPainter(
        text: TextSpan(text: format.format(DateTime(2000, 1, 1, h)), style: label),
        textDirection: TextDirection.ltr,
      )..layout();
      text.paint(canvas, Offset(gutter - 8 - text.width, y - text.height / 2));
      text.dispose();
    }
    if (!now) return;
    final time = clock.value;
    final minutes = _clockMinutes(time);
    final y = inset + minutes / 60 * hour;
    final hourY = inset + (minutes / 60).round() * hour;
    if ((hourY - y).abs() < _nowPillHeight) {
      canvas.drawRect(Rect.fromLTRB(0, hourY - _nowPillHeight / 2, gutter - 4, hourY + _nowPillHeight / 2), Paint()..color = mask);
    }
    final text = TextPainter(text: TextSpan(text: gutterTime(time), style: nowLabel), textDirection: TextDirection.ltr)..layout();
    const padding = 5.0;
    final pill = RRect.fromLTRBR(
      gutter - 3 - text.width - 2 * padding,
      y - _nowPillHeight / 2,
      gutter - 3,
      y + _nowPillHeight / 2,
      const Radius.circular(_nowPillHeight / 2),
    );
    canvas.drawRRect(pill, Paint()..color = color);
    text.paint(canvas, Offset(pill.left + padding, y - text.height / 2));
    text.dispose();
  }

  @override
  bool shouldRepaint(_GutterPainter old) =>
      old.clock != clock ||
      old.now != now ||
      old.line != line ||
      old.label != label ||
      old.color != color ||
      old.mask != mask ||
      old.nowLabel != nowLabel;
}

/// The current time across a page with today: a line with a dot across today's column and a
/// dashed one across the page's other days; repaints on its own as [clock] moves.
class _NowLinePainter extends CustomPainter {
  _NowLinePainter({required this.clock, required this.days, required this.color}) : super(repaint: clock);

  final ValueListenable<DateTime> clock;
  final List<DateTime> days;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final now = clock.value;
    final index = days.indexWhere((day) => day.year == now.year && day.month == now.month && day.day == now.day);
    if (index < 0) return;
    const inset = CalendarGrid.inset, hour = CalendarGrid.hourHeight;
    final column = size.width / days.length;
    final y = inset + _clockMinutes(now) / 60 * hour;
    final left = index * column;

    final trail = Paint()
      ..color = color.withValues(alpha: CalendarMetrics.nowTrailOpacity)
      ..strokeWidth = 1;
    for (var x = 0.0; x < size.width; x += 2 * CalendarMetrics.dash) {
      if (x + CalendarMetrics.dash > left && x < left + column) continue;
      canvas.drawLine(Offset(x, y), Offset(math.min(x + CalendarMetrics.dash, size.width), y), trail);
    }
    final solid = Paint()
      ..color = color
      ..strokeWidth = 1.5;
    canvas.drawLine(Offset(left, y), Offset(left + column, y), solid);
    canvas.drawCircle(Offset(left, y), MarkMetrics.dot / 2, solid);
  }

  @override
  bool shouldRepaint(_NowLinePainter old) => old.clock != clock || old.color != color || !listEquals(old.days, days);
}
