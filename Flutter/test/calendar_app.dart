import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'calendar_fixture.dart';

// The real shell over the synthetic calendar of `calendar_fixture.dart`, opened on the agenda: a
// meeting seven minutes away (Up Next), invitations to answer, guests. Answers stay in memory
// and land after a second, so the one being sent shows; nothing reaches a calendar.
// flutter run -d macos -t test/calendar_app.dart
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  final state = MailState(CalendarMail(answerDelay: const Duration(seconds: 1)));
  await state.start();
  state.showCalendar();
  runApp(MailstreamApp(state: state));
}
