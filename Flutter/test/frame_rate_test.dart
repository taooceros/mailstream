import 'dart:ui' show FrameTiming;

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/frame_rate.dart';

import 'frame_rate_app.dart' show FrameRateMail;

/// Frames whose vsyncs start at `starts` (µs), each with `build` and `raster` µs of work.
List<FrameTiming> frames(Iterable<int> starts, {int build = 2000, int raster = 3000}) => [
  for (final start in starts)
    FrameTiming(
      vsyncStart: start,
      buildStart: start + 100,
      buildFinish: start + 100 + build,
      rasterStart: start + 200 + build,
      rasterFinish: start + 200 + build + raster,
      rasterFinishWallTime: start + 200 + build + raster,
    ),
];

/// Vsyncs `count` frames of `interval` µs apart from `from`.
Iterable<int> every(int interval, int count, {int from = 1000000}) => [for (var i = 0; i < count; i++) from + i * interval];

void main() {
  test('a smooth 120 Hz burst reads 120 fps and its slowest frame', () {
    final counter = FrameRateCounter()..add(frames(every(8333, 121)));
    final reading = counter.take()!;
    expect(reading.fps.round(), 120);
    expect(reading.worst, const Duration(microseconds: 3000));
    expect(reading.idle, isFalse);
  });

  test('dropped frames lower the rate; a slow frame shows as the worst', () {
    // Every other frame misses its vsync: 60 fps on a 120 Hz display.
    final counter = FrameRateCounter()
      ..add(frames(every(16667, 30)))
      ..add(frames([1000000 + 30 * 16667], raster: 21000));
    final reading = counter.take()!;
    expect(reading.fps.round(), 60);
    expect(reading.worst, const Duration(microseconds: 21000));
  });

  test('pauses at rest do not count against the rate', () {
    final counter = FrameRateCounter()
      ..add(frames(every(8333, 10)))
      // Half a second of nothing, then another burst.
      ..add(frames(every(8333, 10, from: 1600000)))
      // Short frames 50 ms apart (sparse input, a lone repaint) were not held up either.
      ..add(frames(every(50000, 5, from: 2000000)));
    expect(counter.take()!.fps.round(), 120);
  });

  test('a long frame holds up the next one and lowers the rate', () {
    // 20 smooth frames, one rastering for 30 ms, the next drawn at the vsync after it.
    final counter = FrameRateCounter()
      ..add(frames(every(8333, 20)))
      ..add(frames([1000000 + 20 * 8333], raster: 30000))
      ..add(frames([1000000 + 20 * 8333 + 5 * 8333]));
    final reading = counter.take()!;
    // 25 refresh periods for 21 intervals.
    expect(reading.fps.round(), (21 * 120 / 25).round());
    expect(reading.worst, const Duration(microseconds: 30000));
  });

  test('with nothing drawn since, the last burst stays, marked idle', () {
    final counter = FrameRateCounter();
    expect(counter.take(), isNull);
    counter.add(frames(every(8333, 20)));
    final burst = counter.take()!;
    // One lone frame (a repaint) a second later, then nothing at all.
    counter.add(frames([3000000]));
    final idle = counter.take()!;
    expect(idle.idle, isTrue);
    expect(idle.fps, burst.fps);
    expect(idle.worst, burst.worst);
    expect(counter.take(), idle);
  });

  testWidgets('the meter shows the engine\'s timings and stops listening once removed', (tester) async {
    await tester.pumpWidget(
      MacosTheme(
        data: MailTheme.macos(Brightness.light),
        child: const Directionality(textDirection: TextDirection.ltr, child: Center(child: FrameRateMeter())),
      ),
    );
    expect(find.text('– fps'), findsOneWidget);

    tester.binding.platformDispatcher.onReportTimings!(frames(every(8333, 61)));
    await tester.pump();
    expect(find.textContaining('120 fps'), findsOneWidget);
    expect(find.textContaining('max 3.0 ms'), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
    // Removed, it no longer hears timings (a disposed meter would throw here).
    tester.binding.platformDispatcher.onReportTimings?.call(frames(every(8333, 10, from: 9000000)));
    await tester.pump(FrameRateMeter.refresh);
    expect(tester.takeException(), isNull);
  });

  testWidgets('the meter shows only while flutter.showFrameRate is on', (tester) async {
    final state = MailState(FrameRateMail());
    await tester.runAsync(state.reloadSettings);
    await tester.pumpWidget(
      MacosTheme(
        data: MailTheme.macos(Brightness.light),
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: FrameRateLayer(state: state, child: const SizedBox.expand()),
        ),
      ),
    );
    expect(state.showFrameRate, isFalse);
    expect(find.byType(FrameRateMeter), findsNothing);

    await tester.runAsync(state.toggleFrameRate);
    await tester.pump();
    expect(state.showFrameRate, isTrue);
    expect(find.byType(FrameRateMeter), findsOneWidget);

    await tester.runAsync(state.toggleFrameRate);
    await tester.pump();
    expect(find.byType(FrameRateMeter), findsNothing);
  });
}
