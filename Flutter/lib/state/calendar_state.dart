import 'dart:async';

import 'package:flutter/foundation.dart';

import 'package:mailstream/mailstream.dart' as engine;

/// One event, by account, calendar and its id there.
typedef EventKey = ({String account, String calendar, String id});

EventKey eventKey(engine.CalendarItem event) => (account: event.account, calendar: event.calendar, id: event.id);

/// The day `time` falls on, local midnight.
DateTime dayOf(DateTime time) => DateTime(time.year, time.month, time.day);

/// `day` as the engine names days: `2026-11-08`.
String dayString(DateTime day) =>
    '${day.year.toString().padLeft(4, '0')}-${day.month.toString().padLeft(2, '0')}-${day.day.toString().padLeft(2, '0')}';

/// How the calendar lays out its events: a list of the days ahead, or one day's or one week's
/// hours side by side.
enum CalendarView { agenda, day, week }

/// The calendar in the window: every account's events from a week back on, the view they show
/// in and the event open beside them. The engine fetches Google Calendar and Outlook every five
/// minutes and keeps what it fetched in mail.db; this keeps what it last answered and reads it
/// again on its `calendar` event, so the calendar shows at once and nothing waits on the network.
class CalendarState extends ChangeNotifier {
  CalendarState(this.mail, {required this.report});

  final engine.Mailstream mail;

  /// Shows an error in the window's snack bar.
  final void Function(String message) report;

  /// Days of events the agenda lists, from today.
  static const days = 60;

  /// Days back the day and week views can show: the engine fetches a week back.
  static const pastDays = 7;

  /// The calendar is in view instead of mail.
  bool active = false;

  /// Notifies only when the calendar comes into or out of view or another event opens: all the
  /// window's layout reads of it. Events and loading notify this state alone.
  Listenable get layout => _layout;
  final _layout = _Signal();

  List<engine.CalendarAccount> accounts = const [];
  List<engine.CalendarItem> events = const [];
  bool loading = false;

  /// Whether `calendar.events` answered once: until then no events is not "nothing planned".
  bool listed = false;

  /// The event beside the calendar.
  EventKey? opened;

  CalendarView view = CalendarView.agenda;

  /// The day the day view shows; the week view shows the week it is in.
  DateTime focus = dayOf(DateTime.now());

  /// The days the engine fetches, from its first up to, not including, `until`; none until it
  /// answers. Other days show no events because none are fetched, not because none are planned.
  ({DateTime from, DateTime until})? synced;

  /// Fires when Today is asked for: each view scrolls to now.
  Listenable get toNow => _toNow;
  final _toNow = _Signal();

  Timer? _change;

  /// The read under way, and the one asked for meanwhile, which runs after it.
  Future<void>? _reading;
  Completer<void>? _queued;

  /// Accounts whose sign-in predates calendar access.
  List<engine.CalendarAccount> get needsSignIn => accounts.where((a) => !a.granted).toList();

  bool get hasCalendars => accounts.any((a) => a.granted);

  /// The accounts that may add events, with a calendar to add them to.
  List<engine.CalendarAccount> get writableAccounts =>
      accounts.where((a) => a.granted && a.writable && a.calendars.any((c) => c.writable)).toList();

  /// This computer's time zone, as the engine names it (`America/Los_Angeles`).
  String timeZone = '';

  engine.CalendarItem? event(EventKey key) => events.where((e) => eventKey(e) == key).firstOrNull;

  engine.CalendarItem? get openedEvent => opened == null ? null : event(opened!);

  /// Called for each engine event.
  void changed(engine.Event event) {
    switch (event) {
      case engine.CalendarEvent() || engine.AccountsEvent():
        _change ??= Timer(const Duration(milliseconds: 150), () {
          _change = null;
          unawaited(reload());
        });
      default:
        return;
    }
  }

  @override
  void dispose() {
    _change?.cancel();
    _layout.dispose();
    _toNow.dispose();
    super.dispose();
  }

  /// Reads the events the engine holds; a request made meanwhile runs once after it, and its
  /// future completes after that read.
  Future<void> reload() {
    if (_reading == null) return _reading = _read();
    return (_queued ??= Completer<void>()).future;
  }

  Future<void> _read() async {
    try {
      // From a week back (the day and week views show the past days the engine fetched); the
      // agenda lists only today on.
      final today = dayOf(DateTime.now());
      final from = DateTime(today.year, today.month, today.day - pastDays);
      final answer = await mail.calendarEvents(from: dayString(from), days: pastDays + days + 1);
      accounts = answer.accounts;
      events = answer.events;
      loading = answer.loading;
      timeZone = answer.timeZone;
      final first = DateTime.tryParse(answer.synced.from), until = DateTime.tryParse(answer.synced.until);
      synced = first == null || until == null ? null : (from: first, until: until);
      listed = true;
      if (opened != null && event(opened!) == null) {
        opened = null;
        _layout.notify();
      }
      notifyListeners();
    } on engine.MailException catch (error) {
      report('Could not read the calendar: ${_describe(error)}');
    }
    final queued = _queued;
    _queued = null;
    if (queued == null) {
      _reading = null;
    } else {
      _reading = _read();
      queued.complete(_reading);
    }
  }

  /// The calendar comes into view, from what the engine holds (it fetches what got old itself).
  void show() {
    if (!active) {
      active = true;
      notifyListeners();
      _layout.notify();
    }
    unawaited(reload());
  }

  /// Mail or chat comes back into view.
  void hide() {
    if (!active) return;
    active = false;
    opened = null;
    notifyListeners();
    _layout.notify();
  }

  /// Fetches every account's events now.
  Future<void> refresh() async {
    try {
      await mail.calendarRefresh();
    } on engine.MailException catch (error) {
      report('Could not refresh the calendar: ${_describe(error)}');
    }
  }

  /// Adds an event to `calendar` of `account`, then opens it. Fails with the engine's error,
  /// for the editor to show.
  Future<engine.CalendarItem> create(String account, String calendar, engine.CalendarDraft draft) async {
    final saved = await mail.calendarCreate(account: account, calendar: calendar, draft: draft);
    await reload();
    open(eventKey(saved));
    return saved;
  }

  /// Changes what `draft` gives of `event`. Fails with the engine's error, for the editor.
  Future<engine.CalendarItem> update(engine.CalendarItem event, engine.CalendarDraft draft) async {
    final saved = await mail.calendarUpdate(account: event.account, calendar: event.calendar, id: event.id, draft: draft);
    await reload();
    return saved;
  }

  /// Deletes `event`, which the person confirmed, telling its guests; closes it.
  Future<void> delete(engine.CalendarItem event) async {
    try {
      await mail.calendarDelete(account: event.account, calendar: event.calendar, id: event.id, confirm: true);
      if (opened == eventKey(event)) close();
      await reload();
    } on engine.MailException catch (error) {
      report('Could not delete "${event.title}": ${_describe(error)}');
    }
  }

  void open(EventKey key) {
    if (opened == key) return;
    opened = key;
    notifyListeners();
    _layout.notify();
  }

  void close() {
    if (opened == null) return;
    opened = null;
    notifyListeners();
    _layout.notify();
  }

  void setView(CalendarView to) {
    if (view == to) return;
    view = to;
    notifyListeners();
  }

  /// The day view's next day or the week view's next week (`step` 1), or the one before (-1).
  void page(int step) {
    if (view == CalendarView.agenda) return;
    final days = view == CalendarView.week ? 7 * step : step;
    focus = DateTime(focus.year, focus.month, focus.day + days);
    notifyListeners();
  }

  /// Back to today, scrolled to now.
  void today() {
    final day = dayOf(DateTime.now());
    if (focus != day) {
      focus = day;
      notifyListeners();
    }
    _toNow.notify();
  }

  /// `day` in the day view.
  void showDay(DateTime day) {
    focus = dayOf(day);
    view = CalendarView.day;
    notifyListeners();
  }

  static String _describe(engine.MailException error) =>
      error is engine.FailedMailException ? error.message : '$error';
}

/// A listenable fired by its owner, for a part of a state that changes apart from the rest.
class _Signal extends ChangeNotifier {
  void notify() => notifyListeners();
}
