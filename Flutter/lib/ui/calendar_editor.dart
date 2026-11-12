import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../platform.dart';
import '../state/mail_state.dart';
import 'calendar.dart';
import 'common.dart';
import 'controls.dart';
import 'design.dart';
import 'glass.dart';
import 'people.dart';

/// Opens the event editor over the window: a new event (from [start], an hour long unless [end]
/// says otherwise; all day with [allDay]) or a change to [event], which the account organizes.
/// Answers the saved event; nothing when cancelled.
Future<engine.CalendarItem?> showEventEditor(
  BuildContext context,
  MailState state, {
  engine.CalendarItem? event,
  DateTime? start,
  DateTime? end,
  bool allDay = false,
}) => showSheet<engine.CalendarItem>(
  context,
  (context) => EventEditor(state: state, event: event, start: start, end: end, allDay: allDay),
);

/// Asks before deleting [event], saying who hears of it, and deletes it when the person agrees.
Future<void> confirmDeleteEvent(BuildContext context, MailState state, engine.CalendarItem event) async {
  final guests = event.attendees.where((p) => p.email?.toLowerCase() != event.account.toLowerCase()).length;
  final message = [
    'It is removed from the “${event.calendarName ?? event.calendar}” calendar.',
    if (event.series != null) 'Only this occurrence is deleted; the rest of the series stays.',
    if (guests == 1) 'Its guest is told it is cancelled.',
    if (guests > 1) 'Its $guests guests are told it is cancelled.',
  ].join(' ');
  final confirmed = await confirmAction(
    context,
    title: 'Delete “${event.title}”?',
    message: message,
    confirm: 'Delete',
  );
  if (confirmed) await state.calendar.delete(event);
}

/// One calendar an event can go on: its account and id.
typedef _CalendarChoice = ({String account, String calendar});

final _day = DateFormat('yyyy-MM-dd');
final _wallClock = DateFormat("yyyy-MM-dd'T'HH:mm");
final _email = RegExp(r'^[^@\s,;<>]+@[^@\s,;<>]+\.[^@\s,;<>]+$');

DateTime _dateOf(DateTime time) => DateTime(time.year, time.month, time.day);

/// The editor's sheet: title, the calendar (a new event's), all day, start and end, where, the
/// guests (completed from contacts) and the description. Saving sends only what changed of an
/// existing event; guests hear of it from the calendar.
class EventEditor extends StatefulWidget {
  const EventEditor({super.key, required this.state, this.event, this.start, this.end, this.allDay = false});

  final MailState state;
  final engine.CalendarItem? event;
  final DateTime? start;
  final DateTime? end;
  final bool allDay;

  @override
  State<EventEditor> createState() => _EventEditorState();
}

class _EventEditorState extends State<EventEditor> {
  late final _title = TextEditingController(text: widget.event?.title ?? '');
  late final _location = TextEditingController(text: widget.event?.location ?? '');
  late final _notes = TextEditingController(text: widget.event?.notes ?? '');
  final _guestField = TextEditingController();
  final _guestFocus = FocusNode(debugLabel: 'Guests');

  /// The calendar a new event goes on.
  _CalendarChoice? _calendar;
  late bool _allDay;

  /// When it starts and ends; all day, their days are the first and the last.
  late DateTime _start, _end;
  late final List<String> _guests;

  /// What the event had, to send only what changed.
  late final String _initialWhen;
  late final List<String> _initialGuests;

  int _revision = 0;
  bool _saving = false;
  String? _error;

  engine.CalendarItem? get event => widget.event;

  @override
  void initState() {
    super.initState();
    final event = this.event;
    if (event == null) {
      final now = DateTime.now();
      final start = widget.start ?? DateTime(now.year, now.month, now.day, now.hour + 1);
      _start = start;
      _end = widget.end ?? start.add(const Duration(hours: 1));
      _allDay = widget.allDay;
      _guests = [];
      final choices = _choices;
      final own = choices.where((c) => _calendarInfo(c)?.primary ?? false);
      _calendar = own.firstOrNull ?? choices.firstOrNull;
    } else {
      final span = eventSpan(event);
      _allDay = event.allDay;
      if (event.allDay) {
        final last = span.end.subtract(const Duration(days: 1));
        _start = DateTime(span.start.year, span.start.month, span.start.day, 9);
        _end = DateTime(last.year, last.month, last.day, 10);
        if (_end.isBefore(_start)) _end = _start.add(const Duration(hours: 1));
      } else {
        _start = span.start;
        _end = span.end;
      }
      _guests = [
        for (final person in event.attendees)
          if (person.email case final email? when email.toLowerCase() != event.account.toLowerCase()) email,
      ];
    }
    _initialWhen = _when.join('|');
    _initialGuests = _normalized(_guests);
    _title.addListener(() => setState(() {}));
    _guestField.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _title.dispose();
    _location.dispose();
    _notes.dispose();
    _guestField.dispose();
    _guestFocus.dispose();
    super.dispose();
  }

  /// Every calendar a new event can go on, account by account.
  List<_CalendarChoice> get _choices => [
    for (final account in widget.state.calendar.writableAccounts)
      for (final calendar in account.calendars)
        if (calendar.writable) (account: account.id, calendar: calendar.id),
  ];

  engine.CalendarInfo? _calendarInfo(_CalendarChoice choice) => widget.state.calendar.accounts
      .where((a) => a.id == choice.account)
      .firstOrNull
      ?.calendars
      .where((c) => c.id == choice.calendar)
      .firstOrNull;

  /// The start, end and all-day as the engine takes them.
  List<String> get _when => _allDay
      ? [_day.format(_start), _day.format(_end), 'all-day']
      : [_wallClock.format(_start), _wallClock.format(_end), 'timed'];

  static List<String> _normalized(List<String> emails) => emails.map((e) => e.toLowerCase()).toList()..sort();

  /// The start moved: the end keeps the length the event had.
  void _moveStart(DateTime start) {
    final length = _end.difference(_start);
    setState(() {
      _start = start;
      _end = start.add(length);
      _error = null;
    });
  }

  void _moveEnd(DateTime end) => setState(() {
    _end = end;
    _error = null;
  });

  /// Takes the typed addresses (split at commas, semicolons and spaces) into the guests.
  bool _commitTyped() {
    final parts = _guestField.text.split(RegExp(r'[,;\s]+')).where((p) => p.isNotEmpty).toList();
    final bad = parts.where((p) => !_email.hasMatch(p)).toList();
    for (final part in parts.where(_email.hasMatch)) {
      _addGuest(part);
    }
    _guestField.text = bad.join(' ');
    if (bad.isNotEmpty) setState(() => _error = '"${bad.first}" is not an email address.');
    return bad.isEmpty;
  }

  void _addGuest(String email) {
    if (_guests.any((g) => g.toLowerCase() == email.toLowerCase())) return;
    setState(() {
      _guests.add(email);
      _revision++;
      _error = null;
    });
  }

  Future<List<Suggestion>> _suggest(String text) async {
    final query = text.trim();
    if (query.isEmpty || _saving) return const [];
    final people = await widget.state.searchContacts(query);
    return [
      for (final person in people)
        if (!_guests.any((g) => g.toLowerCase() == person.email.toLowerCase())) Suggestion(person),
    ];
  }

  /// What to send: everything for a new event, what changed for an existing one.
  engine.CalendarDraft _draft() {
    final title = _title.text.trim();
    final location = _location.text.trim();
    final notes = _notes.text.trimRight();
    final when = _when;
    final event = this.event;
    if (event == null) {
      return engine.CalendarDraft(
        title: title,
        start: when[0],
        end: when[1],
        allDay: _allDay,
        location: location.isEmpty ? null : location,
        notes: notes.isEmpty ? null : notes,
        attendees: _guests.isEmpty ? null : List.of(_guests),
      );
    }
    final timeChanged = when.join('|') != _initialWhen;
    final guestsChanged = _normalized(_guests).join(',') != _initialGuests.join(',');
    return engine.CalendarDraft(
      title: title == event.title ? null : title,
      start: timeChanged ? when[0] : null,
      end: timeChanged ? when[1] : null,
      allDay: timeChanged && _allDay,
      location: location == (event.location ?? '') ? null : location,
      notes: event.notesCut || notes == (event.notes ?? '') ? null : notes,
      attendees: guestsChanged ? List.of(_guests) : null,
    );
  }

  static bool _isEmpty(engine.CalendarDraft draft) =>
      draft.title == null && draft.start == null && draft.location == null && draft.notes == null && draft.attendees == null;

  Future<void> _save() async {
    if (_saving || !_commitTyped()) return;
    if (_title.text.trim().isEmpty) {
      setState(() => _error = 'Give the event a title.');
      return;
    }
    final ends = _allDay ? _dateOf(_end).isBefore(_dateOf(_start)) : !_end.isAfter(_start);
    if (ends) {
      setState(() => _error = 'The event ends before it starts.');
      return;
    }
    final draft = _draft();
    final event = this.event;
    if (event != null && _isEmpty(draft)) {
      Navigator.of(context).pop();
      return;
    }
    final calendar = _calendar;
    if (event == null && calendar == null) return;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      final saved = event == null
          ? await widget.state.calendar.create(calendar!.account, calendar.calendar, draft)
          : await widget.state.calendar.update(event, draft);
      if (mounted) Navigator.of(context).pop(saved);
    } on engine.MailException catch (error) {
      if (!mounted) return;
      setState(() {
        _saving = false;
        _error = describe(error);
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final event = this.event;
    final choices = _choices;
    final provider = widget.state.calendar.accounts.where((a) => a.id == (event?.account ?? _calendar?.account)).firstOrNull?.provider;
    final guestsHear = _guests.isNotEmpty || (event?.attendees.isNotEmpty ?? false);

    Widget row(String label, Widget child, {bool top = false}) => Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: top ? CrossAxisAlignment.start : CrossAxisAlignment.center,
        children: [
          SizedBox(
            width: 76,
            child: Padding(
              padding: EdgeInsets.only(top: top ? 5 : 0),
              child: Text(label, textAlign: TextAlign.right, style: text.body.copyWith(color: colors.secondaryLabel)),
            ),
          ),
          const SizedBox(width: 10),
          Expanded(child: child),
        ],
      ),
    );

    Widget field(TextEditingController controller, String placeholder, {int minLines = 1, int maxLines = 1, bool enabled = true}) =>
        MailTextField(
          controller: controller,
          enabled: enabled && !_saving,
          minLines: minLines,
          maxLines: maxLines,
          placeholder: placeholder,
        );

    // A day, and a time of day every 15 minutes (with the event's own when it falls between).
    Widget moment(DateTime value, ValueChanged<DateTime> onChanged) {
      final minutes = value.hour * 60 + value.minute;
      final times = {for (var m = 0; m < 24 * 60; m += 15) m, minutes}.toList()..sort();
      return Row(children: [
        MailDateButton(value: value, onChanged: _saving ? null : onChanged),
        if (!_allDay) ...[
          const SizedBox(width: Space.s8),
          MailMenuButton<int>(
            value: minutes,
            onChanged: _saving ? null : (m) => onChanged(DateTime(value.year, value.month, value.day, m ~/ 60, m % 60)),
            items: [for (final m in times) MailMenuItem(m, DateFormat.jm().format(DateTime(2000, 1, 1, m ~/ 60, m % 60)))],
          ),
        ],
        const Spacer(),
      ]);
    }

    final calendarRow = event != null
        ? Text(
            [event.calendarName ?? event.calendar, if (event.calendarName != event.account) event.account].join(' · '),
            style: text.body,
          )
        : choices.isEmpty
        ? Text('No calendar here can take new events.', style: text.body.copyWith(color: colors.secondaryLabel))
        : Align(
            alignment: Alignment.centerLeft,
            child: MailMenuButton<_CalendarChoice>(
              value: _calendar,
              onChanged: _saving ? null : (choice) => setState(() => _calendar = choice),
              items: [
                for (final choice in choices)
                  MailMenuItem(
                    choice,
                    [
                      _calendarInfo(choice)?.name ?? choice.calendar,
                      if (choices.any((c) => c.account != choice.account) && _calendarInfo(choice)?.name != choice.account) choice.account,
                    ].join(' · '),
                    leading: MarkDot(hexColor(_calendarInfo(choice)?.color) ?? accountColor(widget.state.accountIndex(choice.account))),
                  ),
              ],
            ),
          );

    return Sheet(
      width: 540,
      title: event == null ? 'New event' : 'Edit event',
      actions: [
        MailButton(label: 'Cancel', size: MailControlSize.large, onPressed: _saving ? null : () => Navigator.of(context).pop()),
        MailButton(
          label: _saving ? 'Saving' : (event == null ? 'Add event' : 'Save'),
          kind: MailButtonKind.primary,
          size: MailControlSize.large,
          busy: _saving,
          onPressed: _title.text.trim().isEmpty || (event == null && _calendar == null) ? null : _save,
        ),
      ],
      child: CallbackShortcuts(
        bindings: {Host.command(LogicalKeyboardKey.enter): _save},
        child: SingleChildScrollView(
          padding: const EdgeInsets.symmetric(horizontal: 20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              row(
                'Title',
                MailTextField(
                  controller: _title,
                  autofocus: true,
                  enabled: !_saving,
                  style: text.body.copyWith(fontWeight: FontWeight.w600),
                  placeholder: 'Event title',
                  onSubmitted: (_) => _save(),
                ),
              ),
              row('Calendar', calendarRow),
              row(
                'All day',
                Align(
                  alignment: Alignment.centerLeft,
                  child: MailCheckbox(
                    value: _allDay,
                    semanticLabel: 'All day',
                    onChanged: _saving
                        ? null
                        : (on) => setState(() {
                            _allDay = on;
                            _error = null;
                          }),
                  ),
                ),
              ),
              row('Starts', moment(_start, _moveStart)),
              row('Ends', moment(_end, _moveEnd)),
              row('Location', field(_location, 'Add a place or a link')),
              row(
                'Guests',
                Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    if (_guests.isNotEmpty) ...[
                      Wrap(spacing: 6, runSpacing: 6, children: [
                        for (final email in _guests)
                          Pill(
                            label: email,
                            onDelete: _saving
                                ? null
                                : () => setState(() {
                                    _guests.remove(email);
                                    _revision++;
                                  }),
                          ),
                      ]),
                      const SizedBox(height: 6),
                    ],
                    CallbackShortcuts(
                      bindings: {const SingleActivator(LogicalKeyboardKey.enter): _commitTyped},
                      child: PeopleCompletion(
                        state: widget.state,
                        controller: _guestField,
                        focusNode: _guestFocus,
                        suggest: _suggest,
                        revision: _revision,
                        onPick: (person) {
                          _addGuest(person.address.email);
                          _guestField.clear();
                        },
                        child: MailTextField(
                          controller: _guestField,
                          focusNode: _guestFocus,
                          enabled: !_saving,
                          placeholder: 'Name or email address',
                        ),
                      ),
                    ),
                  ],
                ),
                top: _guests.isNotEmpty,
              ),
              row(
                'Notes',
                Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    field(_notes, 'Add a description', minLines: 3, maxLines: 8, enabled: !(event?.notesCut ?? false)),
                    if (event?.notesCut ?? false)
                      Padding(
                        padding: const EdgeInsets.only(top: 4),
                        child: Text(
                          'The description is longer than shown here; change it in ${provider == engine.Provider.microsoft ? 'Outlook' : 'Google Calendar'}.',
                          style: text.footnote.copyWith(color: colors.secondaryLabel),
                        ),
                      ),
                  ],
                ),
                top: true,
              ),
              if (guestsHear || widget.state.calendar.timeZone.isNotEmpty || event?.series != null)
                Padding(
                  padding: const EdgeInsets.only(left: 86, bottom: 4),
                  child: Text(
                    [
                      if (event?.series != null) 'Only this occurrence changes.',
                      if (!_allDay && widget.state.calendar.timeZone.isNotEmpty) 'Times in ${widget.state.calendar.timeZone}.',
                      if (guestsHear)
                        '${provider == engine.Provider.microsoft ? 'Outlook' : 'Google Calendar'} emails the '
                            '${event == null ? 'invitation' : 'change'} to the guests.',
                    ].join(' '),
                    style: text.footnote.copyWith(color: colors.secondaryLabel),
                  ),
                ),
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.only(left: 86, top: 6),
                  child: Text(_error!, style: text.body.copyWith(color: colors.red)),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
