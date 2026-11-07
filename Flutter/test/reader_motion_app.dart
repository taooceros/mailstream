import 'dart:async';
import 'dart:io';
import 'dart:math';
import 'dart:ui' show FramePhase, FrameTiming;

import 'package:flutter/gestures.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'html_alignment_app.dart' show AlignmentMail;
import 'reader_fixture.dart';

// Same shell, list, reader and native WebKit composition, with synthetic mail only.
// MAILSTREAM_BENCH=reader-dismiss replays trackpad events through GestureBinding.
// MAILSTREAM_BENCH_OUT is a prefix: each continuous gesture/settle gets its own TSV.
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final mail = AlignmentMail()
    ..rows = [for (var i = 0; i < 1000; i++) conversation(i == 0 ? 'a' : 'synthetic-$i', minute: i)];
  final state = MailState(mail);
  await state.start();
  runApp(MailstreamApp(state: state));
  state.open(state.conversations.first);
  if (Platform.environment['MAILSTREAM_BENCH'] == 'reader-dismiss') {
    await Future<void>.delayed(const Duration(seconds: 12));
    await _measure(state);
  }
}

Future<void> _measure(MailState state) async {
  final scheduler = SchedulerBinding.instance;
  final out = Platform.environment['MAILSTREAM_BENCH_OUT']!;
  final timings = <FrameTiming>[];
  final windows = <({String phase, int start, int end})>[];
  void collect(List<FrameTiming> batch) => timings.addAll(batch);
  scheduler.addTimingsCallback(collect);
  var pointer = 97;
  final size = WidgetsBinding.instance.platformDispatcher.views.first.physicalSize /
      WidgetsBinding.instance.platformDispatcher.views.first.devicePixelRatio;
  final at = Offset(size.width - 160, 300);
  for (var cycle = 0; cycle < 6; cycle++) {
    for (final dismiss in [false, true]) {
      if (state.opened == null) {
        state.open(state.conversations.first);
        await Future<void>.delayed(const Duration(seconds: 2));
      }
      scheduler.scheduleFrame();
      await scheduler.endOfFrame;
      final start = scheduler.currentSystemFrameTimeStamp.inMicroseconds;
      final id = pointer++;
      GestureBinding.instance.handlePointerEvent(PointerPanZoomStartEvent(
        pointer: id, device: id, position: at, timeStamp: scheduler.currentSystemFrameTimeStamp,
      ));
      Duration? previous;
      final done = Completer<void>();
      late final Ticker ticker;
      ticker = Ticker((elapsed) {
        const duration = Duration(milliseconds: 600);
        final fraction = min(1.0, elapsed.inMicroseconds / duration.inMicroseconds);
        final pan = Offset((dismiss ? 430 : 70) * fraction, 0);
        final previousPan = Offset((dismiss ? 430 : 70) * (previous?.inMicroseconds ?? 0) / duration.inMicroseconds, 0);
        GestureBinding.instance.handlePointerEvent(PointerPanZoomUpdateEvent(
          pointer: id, device: id, position: at, timeStamp: scheduler.currentSystemFrameTimeStamp,
          pan: pan, panDelta: pan - previousPan,
        ));
        previous = elapsed;
        if (fraction == 1) {
          GestureBinding.instance.handlePointerEvent(PointerPanZoomEndEvent(
            pointer: id, device: id, position: at, timeStamp: scheduler.currentSystemFrameTimeStamp,
          ));
          ticker.dispose();
          done.complete();
        }
      })..start();
      await done.future;
      final release = scheduler.currentSystemFrameTimeStamp.inMicroseconds;
      await Future<void>.delayed(const Duration(milliseconds: 900));
      final end = scheduler.currentSystemFrameTimeStamp.inMicroseconds;
      final phase = '${dismiss ? 'complete' : 'cancel'}-$cycle';
      windows.add((phase: phase, start: start, end: end));
      windows.add((phase: '$phase-drag', start: start, end: release));
      windows.add((phase: '$phase-settle', start: release, end: end));
      await Future<void>.delayed(const Duration(milliseconds: 300));
    }
  }
  await Future<void>.delayed(const Duration(seconds: 2));
  scheduler.removeTimingsCallback(collect);
  for (final window in windows) {
    final log = StringBuffer('# frontend=flutter scenario=reader-dismiss phase=${window.phase} synthetic=1000 native=WebKit\n');
    for (final timing in timings) {
      final start = timing.timestampInMicroseconds(FramePhase.vsyncStart);
      if (start < window.start || start >= window.end) continue;
      final work = max(timing.buildDuration.inMicroseconds, timing.rasterDuration.inMicroseconds);
      log.writeln('${(start / 1000).toStringAsFixed(3)}\t${(work / 1000).toStringAsFixed(3)}');
    }
    await File('$out-${window.phase}.tsv').writeAsString(log.toString());
  }
  await File('$out.done').writeAsString('reader-dismiss complete; synthetic mail only\n');
}
