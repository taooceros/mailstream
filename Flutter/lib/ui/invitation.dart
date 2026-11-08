import 'dart:async';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../state/mail_state.dart';
import 'design.dart';
import 'format.dart';

/// The answers an invitation takes, as the engine names them, with their buttons' labels.
const _answers = [('accepted', 'Accept'), ('tentative', 'Tentative'), ('declined', 'Decline')];

String _answered(String? response) => switch (response) {
  'accepted' => 'You accepted',
  'tentative' => 'You said maybe',
  'declined' => 'You declined',
  _ => '',
};

/// A meeting invitation in a message: when and where, who organizes it, and the account's answer
/// with Accept / Tentative / Decline. The answer goes through the account's calendar (Google
/// Calendar, Outlook), which tells the organizer; the card follows the calendar, so an answer
/// given here, in the calendar or on another device shows. The message's own words show at once;
/// the calendar's event replaces them when the engine finds it.
class InvitationCard extends StatefulWidget {
  const InvitationCard({super.key, required this.state, required this.conversation, required this.message, required this.invitation});

  final MailState state;
  final String conversation;
  final String message;
  final engine.MeetingInvitation invitation;

  @override
  State<InvitationCard> createState() => _InvitationCardState();
}

class _InvitationCardState extends State<InvitationCard> {
  engine.InvitationStatus? _status;

  /// The answer being sent, until the calendar takes it.
  String? _sending;
  String? _error;
  bool _reading = false, _again = false;

  @override
  void initState() {
    super.initState();
    widget.state.calendar.addListener(_calendarChanged);
    unawaited(_read());
  }

  @override
  void didUpdateWidget(InvitationCard old) {
    super.didUpdateWidget(old);
    if (old.message != widget.message || old.conversation != widget.conversation) {
      _status = null;
      unawaited(_read());
    }
  }

  @override
  void dispose() {
    widget.state.calendar.removeListener(_calendarChanged);
    super.dispose();
  }

  /// The calendar read the engine's events again: its answer may have changed.
  void _calendarChanged() => unawaited(_read());

  Future<void> _read() async {
    if (_reading) {
      _again = true;
      return;
    }
    _reading = true;
    try {
      final status = await widget.state.mail.calendarInvitation(id: widget.conversation, message: widget.message);
      if (mounted) setState(() => _status = status);
    } on engine.MailException catch (error) {
      if (mounted) setState(() => _error = _describe(error));
    } finally {
      _reading = false;
    }
    if (_again && mounted) {
      _again = false;
      await _read();
    }
  }

  Future<void> _respond(String response) async {
    final status = _status;
    final target = status?.target;
    if (status == null || target == null || _sending != null) return;
    setState(() {
      _sending = response;
      _error = null;
    });
    try {
      await widget.state.mail.calendarRespond(
        account: status.account,
        calendar: status.calendar,
        event: target,
        response: response,
        confirm: true,
      );
      await _read();
    } on engine.MailException catch (error) {
      if (mounted) setState(() => _error = 'Could not answer: ${_describe(error)}');
    } finally {
      if (mounted) setState(() => _sending = null);
    }
  }

  static String _describe(engine.MailException error) => error is engine.FailedMailException ? error.message : '$error';

  /// When it is: the calendar's event when found (it follows changes), else the message's words.
  String _when(DateTime now) {
    final event = _status?.event;
    final invitation = widget.invitation;
    final allDay = event?.allDay ?? invitation.allDay;
    final startDay = event?.startDay ?? invitation.startDay, endDay = event?.endDay ?? invitation.endDay;
    String day(DateTime d) => DateFormat(d.year == now.year ? 'EEEE, MMMM d' : 'EEEE, MMMM d, y').format(d);
    if (allDay && startDay != null && endDay != null) {
      final first = DateTime.parse(startDay), last = DateTime.parse(endDay).subtract(const Duration(days: 1));
      return !last.isAfter(first) ? '${day(first)} · All day' : '${day(first)} – ${day(last)}';
    }
    final start = DateTime.parse(event?.start ?? invitation.start).toLocal();
    final end = DateTime.parse(event?.end ?? invitation.end).toLocal();
    final time = DateFormat.jm();
    if (sameDay(start, end)) return '${day(start)} · ${time.format(start)} – ${time.format(end)}';
    return '${day(start)}, ${time.format(start)} – ${day(end)}, ${time.format(end)}';
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final invitation = widget.invitation;
    final status = _status;
    final event = status?.event;
    final cancelled = invitation.method == 'cancel';
    final response = event?.response;
    final organizer = event?.organizer ?? invitation.organizer;
    final location = event?.location ?? invitation.location;
    final secondary = text.body.copyWith(color: colors.secondaryLabel);
    Widget line(IconData icon, String value) => Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Padding(padding: const EdgeInsets.only(top: 1), child: Icon(icon, size: 15, color: colors.secondaryLabel)),
        const SizedBox(width: 8),
        Expanded(child: Text(value, style: text.body)),
      ]),
    );
    // Why it cannot be answered here, when it cannot.
    final String? notice = switch (status) {
      _ when cancelled => 'This meeting was cancelled.',
      null => null,
      engine.InvitationStatus(granted: false) || engine.InvitationStatus(editable: false) =>
        'Sign in to ${status.account} again (Settings › Accounts) to answer from here.',
      engine.InvitationStatus(problem: final problem?) => 'Could not read the calendar: $problem',
      engine.InvitationStatus(event: null) => 'Not in your calendar yet.',
      _ when response == 'organizer' => 'You organized this meeting.',
      _ => null,
    };
    final answerable = !cancelled && status?.target != null && status!.editable && response != 'organizer';
    return Container(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 12),
      decoration: BoxDecoration(
        color: colors.background,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: colors.separator),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(children: [
            Icon(CupertinoIcons.calendar, size: 18, color: cancelled ? colors.red : colors.accent),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                event?.title ?? invitation.title,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: text.headline.copyWith(decoration: cancelled || response == 'declined' ? TextDecoration.lineThrough : null),
              ),
            ),
            if (cancelled)
              _Chip(label: 'Cancelled', color: colors.red)
            else if (_answered(response).isNotEmpty)
              _Chip(label: _answered(response), color: response == 'declined' ? colors.secondaryLabel : colors.accent),
          ]),
          line(CupertinoIcons.clock, [_when(DateTime.now()), if (invitation.recurring) 'Repeats'].join(' · ')),
          if (location != null) line(CupertinoIcons.location, location),
          if (organizer != null && response != 'organizer') line(CupertinoIcons.person, 'Organized by ${_person(organizer)}'),
          if (notice != null) Padding(padding: const EdgeInsets.only(top: 8), child: Text(notice, style: secondary)),
          if (answerable)
            Padding(
              padding: const EdgeInsets.only(top: 10),
              child: Row(children: [
                Text('Going?', style: secondary),
                const SizedBox(width: 10),
                for (final (value, label) in _answers) ...[
                  _AnswerButton(
                    label: label,
                    chosen: (_sending ?? response) == value,
                    sending: _sending == value,
                    onPressed: _sending == null && response != value ? () => _respond(value) : null,
                  ),
                  const SizedBox(width: 6),
                ],
              ]),
            ),
          if (_error != null) Padding(padding: const EdgeInsets.only(top: 8), child: Text(_error!, style: text.body.copyWith(color: colors.red))),
        ],
      ),
    );
  }

  static String _person(engine.CalendarPerson person) => switch ((person.name, person.email)) {
    (final name?, final email?) when name != email => '$name <$email>',
    (final name?, _) => name,
    (_, final email?) => email,
    _ => 'someone',
  };
}

class _Chip extends StatelessWidget {
  const _Chip({required this.label, required this.color});

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
    decoration: BoxDecoration(color: color.withValues(alpha: 0.14), borderRadius: BorderRadius.circular(9)),
    child: Text(label, style: context.text.footnote.copyWith(color: color, fontWeight: FontWeight.w600)),
  );
}

/// One answer: filled when it is the account's (or the one being sent), with a spinner while sent.
class _AnswerButton extends StatelessWidget {
  const _AnswerButton({required this.label, required this.chosen, required this.sending, required this.onPressed});

  final String label;
  final bool chosen, sending;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) => PushButton(
    controlSize: ControlSize.regular,
    secondary: !chosen,
    // The chosen answer stays drawn as chosen while nothing else can be pressed.
    onPressed: onPressed ?? (chosen ? () {} : null),
    child: Row(mainAxisSize: MainAxisSize.min, children: [
      if (sending) ...[const SizedBox(width: 10, height: 10, child: ProgressCircle(radius: 5)), const SizedBox(width: 6)],
      if (chosen && !sending) ...[const MacosIcon(CupertinoIcons.checkmark, size: 12), const SizedBox(width: 4)],
      Text(label),
    ]),
  );
}
