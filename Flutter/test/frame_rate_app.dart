import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// The real shell over 1,000 synthetic conversations, with the frame rate meter's setting kept in
// memory: Settings and View ▸ Show Frame Rate (⌥⌘P) turn it on and off without accounts, private
// mail or settings.json.
// flutter build macos --release -t test/frame_rate_app.dart
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final mail = FrameRateMail()..rows = [for (var i = 0; i < 1000; i++) conversation('synthetic-$i', minute: i)];
  final state = MailState(mail);
  await state.start();
  runApp(MailstreamApp(state: state));
}

class FrameRateMail extends ReaderMail {
  var _shown = false;

  engine.SettingEntry get _entry => engine.SettingEntry(
    key: MailState.frameRateSetting,
    value: engine.FlagSettingValue(_shown),
    defaultValue: engine.FlagSettingValue(false),
    changed: _shown,
    summary:
        'Show the frame rate and the slowest recent frame in a corner of Mailstream Flutter\'s window (also View ▸ Show Frame Rate).',
    choices: const [],
  );

  @override
  Future<engine.Settings> settings() async => engine.Settings(file: '(in memory)', settings: [_entry], issues: []);

  @override
  Future<engine.SettingEntry> setSetting({required String key, required engine.SettingValue value}) async {
    _shown = (value as engine.FlagSettingValue).enabled;
    return _entry;
  }

  @override
  Future<engine.SettingEntry> resetSetting({required String key}) async {
    _shown = false;
    return _entry;
  }
}
