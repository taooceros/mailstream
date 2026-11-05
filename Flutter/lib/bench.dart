import 'dart:async';
import 'dart:io';
import 'dart:math';
import 'dart:ui';

import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

/// The frame measurement `scripts/bench.sh flutter` runs (AGENTS.md, "Performance: 120 fps").
/// With `MAILSTREAM_BENCH=scroll` the conversation list scrolls by itself, every frame's timing
/// goes to `MAILSTREAM_BENCH_OUT` and the app quits.
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
        // Timings arrive in batches: wait for the last frames' before writing.
        Timer(const Duration(seconds: 2), () {
          scheduler.removeTimingsCallback(collect);
          _write(out, timings, first!, last);
          ServicesBinding.instance.exitApplication(AppExitType.required);
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

  /// One line per frame drawn while scrolling: its vsync start and its longer stage (build on
  /// the UI thread or raster), in ms.
  static void _write(String out, List<FrameTiming> timings, Duration first, Duration last) {
    final log = StringBuffer(
      '# frontend=flutter scenario=scroll settle=${settle.inSeconds}s legs=${legs}x${leg.inSeconds}s '
      'distance=${distance.toInt()}px\n',
    );
    for (final timing in timings) {
      final start = timing.timestampInMicroseconds(FramePhase.vsyncStart);
      if (start < first.inMicroseconds || start >= last.inMicroseconds) continue;
      final work = max(timing.buildDuration.inMicroseconds, timing.rasterDuration.inMicroseconds);
      log.writeln('${(start / 1000).toStringAsFixed(3)}\t${(work / 1000).toStringAsFixed(3)}');
    }
    File(out).writeAsStringSync(log.toString());
  }
}
