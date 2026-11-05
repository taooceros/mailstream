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

  static ThemeData _theme(Brightness brightness) {
    final palette = brightness == Brightness.dark ? Palette.dark : Palette.light;
    return ThemeData(
      brightness: brightness,
      useMaterial3: true,
      colorScheme: ColorScheme.fromSeed(seedColor: palette.accent, brightness: brightness, surface: palette.page),
      scaffoldBackgroundColor: palette.page,
      splashFactory: NoSplash.splashFactory,
      highlightColor: const Color(0x00000000),
      textSelectionTheme: TextSelectionThemeData(selectionColor: palette.accent.withValues(alpha: 0.25), cursorColor: palette.accent),
      scrollbarTheme: ScrollbarThemeData(
        thickness: const WidgetStatePropertyAll(6),
        radius: const Radius.circular(3),
        thumbColor: WidgetStatePropertyAll(palette.faint),
      ),
      tooltipTheme: TooltipThemeData(
        textStyle: uiText(TextSize.caption, palette.text),
        decoration: BoxDecoration(
          color: palette.sidebar,
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: palette.line, width: 0.5),
          boxShadow: Shadows.raised,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'Mailstream',
    debugShowCheckedModeBanner: false,
    theme: _theme(Brightness.light),
    darkTheme: _theme(Brightness.dark),
    builder: (context, child) {
      final brightness = MediaQuery.platformBrightnessOf(context);
      return DesignScope(
        palette: brightness == Brightness.dark ? Palette.dark : Palette.light,
        brightness: brightness,
        child: child!,
      );
    },
    home: Material(type: MaterialType.transparency, child: MailShell(state: state)),
  );
}
