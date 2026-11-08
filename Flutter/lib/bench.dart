import 'dart:async';
import 'dart:developer' show Timeline;
import 'dart:io';
import 'dart:math';
import 'dart:ui';

import 'package:flutter/gestures.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'state/mail_state.dart';

/// The frame measurements `scripts/bench.sh flutter` runs (AGENTS.md, "Performance: 120 fps").
/// With `MAILSTREAM_BENCH=scroll` the conversation list scrolls by itself, every frame's timing
/// goes to `MAILSTREAM_BENCH_OUT` and the app quits; `MAILSTREAM_BENCH=reader-swipe` is
/// [ReaderDismissBench].
abstract final class ScrollBench {
  /// The scenario every front end runs: after [settle], [legs] legs of [leg] each, down
  /// [distance] and back, the offset set from the time each frame.
  static const settle = Duration(seconds: 12);
  static const legs = 8;
  static const leg = Duration(seconds: 3);
  static const distance = 4000.0;

  static bool _attached = false;

  /// Runs the measurement on the first list attached, if the environment asks for one.
  static void attach(ScrollController list) {
    final out = Platform.environment['MAILSTREAM_BENCH_OUT'];
    if (_attached || Platform.environment['MAILSTREAM_BENCH'] != 'scroll' || out == null) return;
    _attached = true;
    Timer(settle, () {
      // Until the list has rows to scroll.
      Timer.periodic(const Duration(milliseconds: 500), (wait) {
        if (!list.hasClients || list.position.maxScrollExtent <= 0) return;
        wait.cancel();
        _run(list, out);
      });
    });
  }

  static void _run(ScrollController list, String out) {
    final scheduler = SchedulerBinding.instance;
    final timings = <FrameTiming>[];
    void collect(List<FrameTiming> batch) => timings.addAll(batch);
    scheduler.addTimingsCallback(collect);
    Duration? first;
    late final Ticker ticker;
    ticker = Ticker((elapsed) {
      first ??= scheduler.currentSystemFrameTimeStamp;
      if (elapsed >= leg * legs) {
        final last = scheduler.currentSystemFrameTimeStamp;
        ticker.dispose();
        _finish(() {
          scheduler.removeTimingsCallback(collect);
          final log = StringBuffer(
            '# frontend=flutter scenario=scroll settle=${settle.inSeconds}s legs=${legs}x${leg.inSeconds}s '
            'distance=${distance.toInt()}px\n',
          );
          _frames(log, timings, first!.inMicroseconds, last.inMicroseconds);
          File(out).writeAsStringSync(log.toString());
        });
        return;
      }
      // A triangle wave: down on even legs, up on odd ones.
      final at = elapsed.inMicroseconds / leg.inMicroseconds;
      final along = at - at.floor();
      final depth = at.floor().isEven ? along : 1 - along;
      if (list.hasClients) list.jumpTo(min(depth * distance, list.position.maxScrollExtent));
    });
    ticker.start();
  }
}

/// `MAILSTREAM_BENCH=reader-swipe`: after [ScrollBench.settle], opens [rounds] conversations
/// from the top of the list one after another, as clicking their rows does, and after [read]
/// swipes each reader away with two fingers on the trackpad: the gesture's events go through
/// [GestureBinding] as the system's do. The fingers land [rest] before they move (macOS starts the
/// pan when they touch), then move [distance] in [swipe], and the reader flies off on the swipe's
/// spring. The log holds the frames from the fingers landing until the reader has gone, each swipe
/// a `# segment` of its own (the app rests between them).
abstract final class ReaderDismissBench {
  static const rounds = 8;
  static const rest = Duration(milliseconds: 100);
  static const swipe = Duration(milliseconds: 280);
  static const distance = 360.0;

  /// Long enough for the reader to slide in, its bodies to load and the window to rest.
  static const read = Duration(seconds: 2);

  static bool _attached = false;

  /// Runs the measurement on the window's state, if the environment asks for it.
  static void attach(MailState state) {
    final out = Platform.environment['MAILSTREAM_BENCH_OUT'];
    if (_attached || Platform.environment['MAILSTREAM_BENCH'] != 'reader-swipe' || out == null) return;
    _attached = true;
    Timer(ScrollBench.settle, () {
      // Until the list has its conversations.
      Timer.periodic(const Duration(milliseconds: 500), (wait) {
        if (state.conversations.length < rounds) return;
        wait.cancel();
        _run(state, out);
      });
    });
  }

  static Future<void> _run(MailState state, String out) async {
    final scheduler = SchedulerBinding.instance;
    final timings = <FrameTiming>[];
    void collect(List<FrameTiming> batch) => timings.addAll(batch);
    scheduler.addTimingsCallback(collect);
    final view = PlatformDispatcher.instance.views.first;
    final size = view.physicalSize / view.devicePixelRatio;
    // On the reader in either layout: it fills the window's trailing side.
    final at = Offset(size.width - 200, size.height / 2);
    final segments = <(int, int)>[];
    for (var round = 0; round < rounds; round++) {
      state.open(state.conversations[round]);
      await Future<void>.delayed(read);
      // The first frame after the rest of reading. Timeline's clock is the frame timings'.
      final start = Timeline.now;
      final pointer = 1000 + round;
      GestureBinding.instance.handlePointerEvent(PointerPanZoomStartEvent(
        pointer: pointer,
        device: pointer,
        position: at,
        timeStamp: Duration(microseconds: start),
      ));
      await Future<void>.delayed(rest);
      final released = Completer<void>();
      var pan = Offset.zero;
      late final Ticker ticker;
      ticker = Ticker((elapsed) {
        final stamp = scheduler.currentSystemFrameTimeStamp;
        final fraction = min(1.0, elapsed.inMicroseconds / swipe.inMicroseconds);
        final next = Offset(distance * fraction, 0);
        GestureBinding.instance.handlePointerEvent(PointerPanZoomUpdateEvent(
          pointer: pointer,
          device: pointer,
          position: at,
          timeStamp: stamp,
          pan: next,
          panDelta: next - pan,
        ));
        pan = next;
        if (fraction < 1) return;
        GestureBinding.instance.handlePointerEvent(
          PointerPanZoomEndEvent(pointer: pointer, device: pointer, position: at, timeStamp: stamp),
        );
        ticker.dispose();
        released.complete();
      });
      ticker.start();
      await released.future;
      // The spring hands over once the reader is off screen; the next frame removes it.
      while (state.opened != null) {
        await scheduler.endOfFrame;
      }
      await scheduler.endOfFrame;
      segments.add((start, scheduler.currentSystemFrameTimeStamp.inMicroseconds));
      await Future<void>.delayed(const Duration(seconds: 1));
    }
    _finish(() {
      scheduler.removeTimingsCallback(collect);
      final log = StringBuffer(
        '# frontend=flutter scenario=reader-swipe rounds=$rounds rest=${rest.inMilliseconds}ms '
        'swipe=${distance.toInt()}px/${swipe.inMilliseconds}ms\n',
      );
      for (final (first, last) in segments) {
        log.writeln('# segment');
        _frames(log, timings, first, last + 1);
      }
      File(out).writeAsStringSync(log.toString());
    });
  }
}

/// Writes the log once the last frames' timings, which arrive in batches, are in, and quits.
void _finish(void Function() write) {
  Timer(const Duration(seconds: 2), () {
    write();
    ServicesBinding.instance.exitApplication(AppExitType.required);
  });
}

/// One line per frame whose vsync started in [first, end) µs: its vsync start and its longer
/// stage (build on the UI thread or raster), in ms.
void _frames(StringBuffer log, List<FrameTiming> timings, int first, int end) {
  for (final timing in timings) {
    final start = timing.timestampInMicroseconds(FramePhase.vsyncStart);
    if (start < first || start >= end) continue;
    final work = max(timing.buildDuration.inMicroseconds, timing.rasterDuration.inMicroseconds);
    log.writeln('${(start / 1000).toStringAsFixed(3)}\t${(work / 1000).toStringAsFixed(3)}');
  }
}
