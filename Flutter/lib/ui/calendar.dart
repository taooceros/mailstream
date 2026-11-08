import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../platform.dart';
import '../state/calendar_state.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'conversation_list.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';

/// An agenda day's events, all-day ones first.
typedef AgendaDay = ({DateTime day, List<engine.CalendarItem> events});

DateTime _dayOf(DateTime time) => DateTime(time.year, time.month, time.day);

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
    var day = _dayOf(span.start);
    if (day.isBefore(today)) day = today;
    // Ends at midnight: the day before is its last; a moment-long event still shows on its day.
    final last = span.end.isAfter(span.start) ? _dayOf(span.end.subtract(const Duration(microseconds: 1))) : _dayOf(span.start);
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

/// An agenda day's header: Today, Tomorrow, else the weekday and date.
String agendaTitle(DateTime day, DateTime today) {
  if (sameDay(day, today)) return 'Today · ${DateFormat('EEEE, MMMM d').format(day)}';
  if (sameDay(day, today.add(const Duration(days: 1)))) return 'Tomorrow · ${DateFormat('EEEE, MMMM d').format(day)}';
  if (day.year == today.year) return DateFormat('EEEE, MMMM d').format(day);
  return DateFormat('EEEE, MMMM d, y').format(day);
}

/// `#rrggbb` as a color.
Color? hexColor(String? hex) {
  if (hex == null || !hex.startsWith('#') || hex.length != 7) return null;
  final value = int.tryParse(hex.substring(1), radix: 16);
  return value == null ? null : Color(0xFF000000 | value);
}

/// The event's color: its calendar's, else its account's.
Color eventColor(MailState state, engine.CalendarItem event) =>
    hexColor(event.color) ?? accountPalette[state.accountIndex(event.account) % accountPalette.length];

String _responseLabel(String? response) => switch (response) {
  'accepted' => 'Going',
  'tentative' => 'Maybe',
  'declined' => 'Declined',
  'needsAction' => 'Invited',
  'organizer' => 'Organizer',
  _ => '',
};

sealed class _Entry {}

final class _Day extends _Entry {
  _Day(this.title);
  final String title;
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

/// The calendar's list pane: header with refresh, notices for accounts to sign in again or whose
/// fetch failed, then the agenda from today: each day with events under its header.
class CalendarListPane extends StatefulWidget {
  const CalendarListPane({super.key, required this.state});

  final MailState state;

  static const rowHeight = 52.0;
  static const noticeHeight = 64.0;

  @override
  State<CalendarListPane> createState() => _CalendarListPaneState();
}

class _CalendarListPaneState extends State<CalendarListPane> {
  final _scroll = ScrollController();

  /// The account being signed in to again, while the browser is open.
  String? _signingIn;

  CalendarState get calendar => widget.state.calendar;

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
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

  List<_Entry> _entries(DateTime today) {
    final out = <_Entry>[
      for (final account in calendar.accounts)
        if (!account.granted)
          _Notice(account)
        else if (account.problem != null)
          _Notice(account, problem: account.problem),
    ];
    for (final day in agendaDays(calendar.events, today)) {
      out.add(_Day(agendaTitle(day.day, today)));
      for (final event in day.events) {
        out.add(_Event(event, day.day));
      }
    }
    return out;
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final now = DateTime.now();
    final today = _dayOf(now);
    final entries = _entries(today);
    final hasEvents = entries.any((e) => e is _Event);
    final Widget body;
    if (!calendar.listed) {
      body = const Center(child: Spinner());
    } else {
      final empty = hasEvents
          ? null
          : calendar.hasCalendars
          ? (calendar.loading ? null : 'Nothing on your calendars in the next ${CalendarState.days} days')
          : 'No account has calendar access yet. Sign in again to show its calendar here.';
      // MacosApp's scroll behavior puts the Mac's overlay scrollbar on it.
      body = ListView.builder(
        controller: _scroll,
        padding: const EdgeInsets.only(top: 4, bottom: 24),
        itemCount: entries.length + 1,
        itemExtentBuilder: (index, _) => index == entries.length
            ? (empty == null && hasEvents ? 0 : 120)
            : switch (entries[index]) {
                _Day() => ListMetrics.sectionHeight,
                _Event() => CalendarListPane.rowHeight,
                _Notice() => CalendarListPane.noticeHeight,
              },
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
            _Day(:final title) => SectionHeader(title),
            _Event(:final event, :final day) => _EventRow(
              key: ValueKey('${event.account}|${event.calendar}|${event.id}|${day.millisecondsSinceEpoch}'),
              state: widget.state,
              event: event,
              day: day,
              now: now,
              selected: calendar.opened == eventKey(event),
              onTap: () => calendar.open(eventKey(event)),
            ),
            _Notice(:final account, :final problem) => _NoticeRow(
              account: account,
              problem: problem,
              signingIn: _signingIn == account.id,
              onSignIn: _signingIn == null ? () => _signIn(account) : null,
            ),
          };
        },
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: SizedBox(
            height: 56,
            child: Row(children: [
              const SizedBox(width: 18),
              Expanded(
                child: Column(
                  mainAxisAlignment: MainAxisAlignment.center,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(children: [
                      Text('Calendar', style: context.text.title3.copyWith(fontWeight: FontWeight.w700)),
                      if (calendar.loading) const Padding(padding: EdgeInsets.only(left: 8), child: Spinner(radius: 6)),
                    ]),
                    Text(DateFormat('EEEE, MMMM d').format(now), style: context.text.subheadline.copyWith(color: colors.secondaryLabel)),
                  ],
                ),
              ),
              GlassCapsule(
                child: Row(mainAxisSize: MainAxisSize.min, children: [
                  ToolButton(
                    tooltip: 'Today',
                    icon: CupertinoIcons.calendar_today,
                    onPressed: () {
                      if (_scroll.hasClients) _scroll.animateTo(0, duration: Motion.medium, curve: Motion.curve);
                    },
                  ),
                  ToolButton(
                    tooltip: 'Refresh (${Host.shortcut('R')})',
                    icon: CupertinoIcons.arrow_clockwise,
                    onPressed: calendar.hasCalendars ? calendar.refresh : null,
                  ),
                ]),
              ),
              const SizedBox(width: 12),
            ]),
          ),
        ),
        Expanded(child: ClipRect(child: body)),
      ],
    );
  }
}

/// One event in the agenda: its calendar's color bar, the time (or All day), the title and where
/// it is, and the account's answer when it is an invitation.
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

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final span = eventSpan(event);
    final past = !span.end.isAfter(now);
    final declined = event.response == 'declined';
    final dim = past || declined;
    // A timed event shows its times on the day it starts; days it runs through show All day, and
    // the day it ends, when it ends.
    final startsToday = !event.allDay && sameDay(span.start, day);
    final endsToday = !event.allDay && (sameDay(span.end, day) || span.end == DateTime(day.year, day.month, day.day + 1));
    final time = startsToday ? DateFormat.jm().format(span.start) : 'All day';
    final until = startsToday && endsToday
        ? DateFormat.jm().format(span.end)
        : endsToday && !startsToday
        ? 'until ${DateFormat.jm().format(span.end)}'
        : null;
    final detail = [
      if (event.location != null) event.location!,
      if (event.location == null && event.meetingUrl != null) 'Video call',
      if (state.calendar.accounts.where((a) => a.granted).length > 1 || (event.calendarName != null && event.calendarName != event.account))
        event.calendarName ?? event.account,
    ].join(' · ');
    final response = event.response == 'needsAction' || event.response == 'tentative' ? _responseLabel(event.response) : '';
    final secondary = dim ? colors.secondaryLabel.withValues(alpha: 0.6) : colors.secondaryLabel;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset),
      child: LiftedRow(
        selected: selected,
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12),
          child: Row(children: [
            Container(
              width: 4,
              height: 34,
              decoration: BoxDecoration(
                color: eventColor(state, event).withValues(alpha: dim ? 0.4 : 1),
                borderRadius: BorderRadius.circular(2),
              ),
            ),
            const SizedBox(width: 10),
            SizedBox(
              width: 76,
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(time, maxLines: 1, style: context.text.callout.copyWith(color: dim ? secondary : colors.label, fontWeight: FontWeight.w500)),
                  if (until != null) Text(until, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.footnote.copyWith(color: secondary)),
                ],
              ),
            ),
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
                      color: dim ? secondary : colors.label,
                      decoration: declined ? TextDecoration.lineThrough : null,
                    ),
                  ),
                  if (detail.isNotEmpty)
                    Text(detail, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.callout.copyWith(color: secondary)),
                ],
              ),
            ),
            if (event.meetingUrl != null) ...[
              const SizedBox(width: 6),
              Icon(CupertinoIcons.videocam, size: 16, color: secondary),
            ],
            if (response.isNotEmpty) ...[
              const SizedBox(width: 6),
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
                decoration: BoxDecoration(color: colors.accentFill, borderRadius: BorderRadius.circular(9)),
                child: Text(response, style: context.text.footnote.copyWith(color: colors.onAccentFill, fontWeight: FontWeight.w600)),
              ),
            ],
          ]),
        ),
      ),
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
    final colors = context.colors;
    final failed = problem != null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12),
        decoration: BoxDecoration(color: failed ? colors.redFill : colors.waitingFill, borderRadius: BorderRadius.circular(Panes.corner)),
        child: Row(children: [
          Icon(failed ? CupertinoIcons.exclamationmark_triangle : CupertinoIcons.calendar_badge_plus, size: 18,
              color: failed ? colors.onRedFill : colors.onWaitingFill),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(account.label.isEmpty ? account.id : account.label, maxLines: 1, overflow: TextOverflow.ellipsis,
                    style: context.text.body.copyWith(fontWeight: FontWeight.w600)),
                Text(
                  failed ? 'Could not update: $problem' : 'Sign in again to allow calendar access.',
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: context.text.footnote.copyWith(color: colors.secondaryLabel),
                ),
              ],
            ),
          ),
          if (!failed) ...[
            const SizedBox(width: 8),
            if (signingIn) const Padding(padding: EdgeInsets.only(right: 8), child: Spinner(radius: 7)),
            PushButton(controlSize: ControlSize.regular, secondary: true, onPressed: onSignIn, child: const Text('Sign In…')),
          ],
        ]),
      ),
    );
  }
}

/// The open event: when and where, joining its call, the account's answer, who organizes and who
/// is invited, its description, and the provider's page for it.
class CalendarEventPane extends StatelessWidget {
  const CalendarEventPane({super.key, required this.state, required this.event, required this.twoPane, required this.onBack});

  final MailState state;
  final engine.CalendarItem event;
  final bool twoPane;
  final VoidCallback onBack;

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

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final provider = state.calendar.accounts.where((a) => a.id == event.account).firstOrNull?.provider;
    final response = _responseLabel(event.response);
    Widget line(IconData icon, Widget child) => Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Padding(padding: const EdgeInsets.only(top: 1), child: Icon(icon, size: 16, color: colors.secondaryLabel)),
        const SizedBox(width: 10),
        Expanded(child: child),
      ]),
    );
    final text = context.text.body;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: SizedBox(
            height: 52,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: Row(children: [
                GlassCapsule(
                  child: ToolButton(
                    tooltip: '${twoPane ? 'Close' : 'Back'} (Esc or ${Host.shortcut('W')})',
                    icon: twoPane ? CupertinoIcons.xmark : CupertinoIcons.chevron_left,
                    onPressed: onBack,
                  ),
                ),
                const Spacer(),
                if (event.link != null)
                  GlassCapsule(
                    child: ToolButton(
                      tooltip: provider == 'microsoft' ? 'Open in Outlook' : 'Open in Google Calendar',
                      icon: CupertinoIcons.arrow_up_right_square,
                      onPressed: () => state.openExternal(event.link!),
                    ),
                  ),
              ]),
            ),
          ),
        ),
        const Hairline(),
        Expanded(
          child: Selectable(
            child: ListView(
              padding: const EdgeInsets.fromLTRB(24, 18, 24, 24),
              children: [
                Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
                  Container(
                    margin: const EdgeInsets.only(top: 6, right: 10),
                    width: 12,
                    height: 12,
                    decoration: BoxDecoration(color: eventColor(state, event), borderRadius: BorderRadius.circular(3)),
                  ),
                  Expanded(
                    child: Text(
                      event.title,
                      style: context.text.title2.copyWith(
                        fontWeight: FontWeight.w700,
                        decoration: event.response == 'declined' ? TextDecoration.lineThrough : null,
                      ),
                    ),
                  ),
                ]),
                const SizedBox(height: 6),
                Text(_when(DateTime.now()), style: text.copyWith(color: colors.secondaryLabel)),
                const SizedBox(height: 16),
                if (event.meetingUrl != null)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 14),
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: PushButton(
                        controlSize: ControlSize.large,
                        onPressed: () => state.openExternal(event.meetingUrl!),
                        child: const Row(mainAxisSize: MainAxisSize.min, children: [
                          Icon(CupertinoIcons.videocam_fill, size: 16, color: MacosColors.white),
                          SizedBox(width: 6),
                          Text('Join call'),
                        ]),
                      ),
                    ),
                  ),
                if (event.location != null) line(CupertinoIcons.location, Text(event.location!, style: text)),
                line(
                  CupertinoIcons.calendar,
                  Text([event.calendarName ?? event.calendar, if (event.calendarName != event.account) event.account].join(' · '), style: text),
                ),
                if (response.isNotEmpty) line(CupertinoIcons.checkmark_circle, Text(response, style: text)),
                if (event.organizer != null && event.response != 'organizer')
                  line(CupertinoIcons.person, Text('Organized by ${_person(event.organizer!)}', style: text)),
                if (event.attendees.isNotEmpty)
                  line(
                    CupertinoIcons.person_2,
                    Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text('${event.attendees.length} guests', style: text.copyWith(fontWeight: FontWeight.w600)),
                        const SizedBox(height: 4),
                        for (final person in event.attendees)
                          Padding(
                            padding: const EdgeInsets.only(bottom: 3),
                            child: Row(children: [
                              Expanded(child: Text(_person(person), maxLines: 1, overflow: TextOverflow.ellipsis, style: text)),
                              Text(_responseLabel(person.response), style: context.text.footnote.copyWith(color: colors.secondaryLabel)),
                            ]),
                          ),
                      ],
                    ),
                  ),
                if (event.notes != null) ...[
                  const SizedBox(height: 6),
                  const Hairline(),
                  const SizedBox(height: 14),
                  Text(event.notes!, style: text),
                ],
              ],
            ),
          ),
        ),
      ],
    );
  }

  static String _person(engine.CalendarPerson person) {
    final name = person.name, email = person.email;
    if (name != null && email != null && name != email) return '$name <$email>';
    return name ?? email ?? '';
  }
}
