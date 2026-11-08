import 'dart:io' show Platform;

import 'package:flutter/material.dart' show DefaultMaterialLocalizations;
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import 'platform.dart';
import 'state/mail_state.dart';
import 'ui/design.dart';
import 'ui/frame_rate.dart';
import 'ui/reader_renderer.dart';
import 'ui/shell.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // macos_ui's controls dim with the window and follow the accent color through the window
  // delegate macos_window_utils installs. The plugin is the Mac's alone.
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  // The engine starts in this process: it paints from the shared cache and syncs every account.
  final state = MailState(engine.Mailstream())..start();
  runApp(MailstreamApp(state: state));
}

class MailstreamApp extends StatefulWidget {
  const MailstreamApp({super.key, required this.state});

  final MailState state;

  @override
  State<MailstreamApp> createState() => _MailstreamAppState();
}

class _MailstreamAppState extends State<MailstreamApp> {
  /// The reader's renderer: HyperRender on every launch, switched only by hand
  /// (`MAILSTREAM_READER_RENDERER` picks another for the bench).
  final _renderer = ValueNotifier(ReaderRenderer.fromName(Platform.environment['MAILSTREAM_READER_RENDERER']));

  @override
  void dispose() {
    _renderer.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => MacosApp(
    title: 'Mailstream',
    debugShowCheckedModeBanner: false,
    // HyperRender's selection menus ask for Material's strings.
    localizationsDelegates: const [DefaultMaterialLocalizations.delegate],
    theme: MailTheme.macos(Brightness.light),
    darkTheme: MailTheme.macos(Brightness.dark),
    home: ReaderRendererScope(controller: _renderer, child: MailShell(state: widget.state)),
    // On the Mac the window's glass shows around the islands; elsewhere nothing is under the
    // Flutter view, so the app paints the window's color itself.
    builder: (context, child) {
      final layer = FrameRateLayer(state: widget.state, child: child!);
      return Host.mac ? layer : ColoredBox(color: context.colors.window, child: layer);
    },
  );
}
