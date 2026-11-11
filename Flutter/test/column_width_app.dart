import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/slide_layer.dart';

import 'calendar_fixture.dart';

// The real shell over the synthetic calendar and mail of `calendar_fixture.dart`, opened on the
// calendar, with `flutter.listWidth` and `flutter.calendarWidth` kept in memory: dragging the gap
// beside an open event or conversation (MS-185) keeps its width here instead of in settings.json.
// Writing a number of seconds to /tmp/ms185-sweep drags the open split's gap back and forth for
// that long with synthetic mouse events at 240 Hz, for mid-drag screenshots and the frame rate
// meter, without taking the real pointer.
// flutter build macos --release -t test/column_width_app.dart
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  final state = MailState(ColumnWidthMail());
  await state.start();
  state.showCalendar();
  runApp(MailstreamApp(state: state));
  Timer.periodic(const Duration(milliseconds: 300), (_) => _sweepIfAsked());
}

const _trigger = '/tmp/ms185-sweep';
var _sweeping = false;

Future<void> _sweepIfAsked() async {
  final file = File(_trigger);
  if (_sweeping || !file.existsSync()) return;
  final seconds = double.tryParse(file.readAsStringSync().trim()) ?? 5;
  file.deleteSync();
  Element? gap;
  void visit(Element element) {
    if (element.widget is ResizeGap) gap = element;
    if (gap == null) element.visitChildren(visit);
  }

  WidgetsBinding.instance.rootElement?.visitChildren(visit);
  final box = gap?.renderObject as RenderBox?;
  if (box == null) return;
  _sweeping = true;
  final from = box.localToGlobal(box.size.center(Offset.zero));
  final binding = GestureBinding.instance;
  const pointer = 7340;
  binding.handlePointerEvent(PointerAddedEvent(pointer: pointer, kind: PointerDeviceKind.mouse, position: from));
  binding.handlePointerEvent(
    PointerDownEvent(pointer: pointer, kind: PointerDeviceKind.mouse, buttons: kPrimaryButton, position: from),
  );
  final clock = Stopwatch()..start();
  var at = from;
  final done = Completer<void>();
  Timer.periodic(const Duration(microseconds: 4167), (timer) {
    final t = clock.elapsedMicroseconds / 1e6;
    final next = from + Offset(260 * math.sin(t * math.pi / 2), 0);
    if (t >= seconds) {
      timer.cancel();
      binding.handlePointerEvent(PointerUpEvent(pointer: pointer, kind: PointerDeviceKind.mouse, position: at));
      done.complete();
      return;
    }
    binding.handlePointerEvent(
      PointerMoveEvent(
        pointer: pointer,
        kind: PointerDeviceKind.mouse,
        buttons: kPrimaryButton,
        position: next,
        delta: next - at,
      ),
    );
    at = next;
  });
  await done.future;
  _sweeping = false;
}

class ColumnWidthMail extends CalendarMail {
  static const defaults = {MailState.listWidthSetting: 400, MailState.calendarWidthSetting: 500};

  /// The widths set, in thousandths; absent keys hold their default.
  final widths = <String, int>{};

  engine.SettingEntry entry(String key) => engine.SettingEntry(
    key: key,
    value: engine.NumberSettingValue(widths[key] ?? defaults[key]!),
    defaultValue: engine.NumberSettingValue(defaults[key]!),
    changed: widths.containsKey(key),
    summary: 'Width in thousandths of the window beside the sidebar.',
    minimum: 100,
    maximum: 900,
    choices: const [],
  );

  /// `flutter.showFrameRate` too (View ▸ Show Frame Rate, ⌥⌘P), to watch a drag's frame rate.
  var frameRate = false;

  engine.SettingEntry get frameRateEntry => engine.SettingEntry(
    key: MailState.frameRateSetting,
    value: engine.FlagSettingValue(frameRate),
    defaultValue: engine.FlagSettingValue(false),
    changed: frameRate,
    summary: 'Show the frame rate and the slowest recent frame.',
    choices: const [],
  );

  @override
  Future<engine.Settings> settings() async => engine.Settings(
    file: '(in memory)',
    settings: [for (final key in defaults.keys) entry(key), frameRateEntry],
    issues: [],
  );

  @override
  Future<engine.SettingEntry> setSetting({required String key, required engine.SettingValue value}) async {
    if (value case engine.FlagSettingValue(:final enabled)) {
      frameRate = enabled;
      return frameRateEntry;
    }
    widths[key] = (value as engine.NumberSettingValue).value;
    return entry(key);
  }

  @override
  Future<engine.SettingEntry> resetSetting({required String key}) async {
    if (key == MailState.frameRateSetting) {
      frameRate = false;
      return frameRateEntry;
    }
    widths.remove(key);
    return entry(key);
  }
}
