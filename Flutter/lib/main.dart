import 'package:flutter/material.dart';

import 'backend/control_client.dart';
import 'state/mail_state.dart';
import 'ui/design.dart';
import 'ui/shell.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  final state = MailState(ControlClient())..start();
  runApp(MailstreamApp(state: state));
}

class MailstreamApp extends StatelessWidget {
  const MailstreamApp({super.key, required this.state});

  final MailState state;

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'Mailstream',
    debugShowCheckedModeBanner: false,
    theme: MailTheme.of(Brightness.light),
    darkTheme: MailTheme.of(Brightness.dark),
    home: MailShell(state: state),
  );
}
