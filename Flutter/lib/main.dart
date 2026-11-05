import 'package:flutter/material.dart';

import 'package:mailstream/mailstream.dart' as engine;

import 'state/mail_state.dart';
import 'ui/design.dart';
import 'ui/shell.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  // The engine starts in this process: it paints from the shared cache and syncs every account.
  final state = MailState(engine.Mailstream())..start();
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
