import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import 'state/mail_state.dart';
import 'ui/design.dart';
import 'ui/frame_rate.dart';
import 'ui/shell.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // macos_ui's controls dim with the window and follow the accent color through the window
  // delegate macos_window_utils installs.
  await WindowManipulator.initialize(enableWindowDelegate: true);
  // The engine starts in this process: it paints from the shared cache and syncs every account.
  final state = MailState(engine.Mailstream())..start();
  runApp(MailstreamApp(state: state));
}

class MailstreamApp extends StatelessWidget {
  const MailstreamApp({super.key, required this.state});

  final MailState state;

  @override
  Widget build(BuildContext context) => MacosApp(
    title: 'Mailstream',
    debugShowCheckedModeBanner: false,
    theme: MailTheme.macos(Brightness.light),
    darkTheme: MailTheme.macos(Brightness.dark),
    home: MailShell(state: state),
    builder: (context, child) => FrameRateLayer(state: state, child: child!),
  );
}
