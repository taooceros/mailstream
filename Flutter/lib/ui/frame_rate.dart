import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' show FontFeature, FramePhase, FrameTiming;

import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import '../state/mail_state.dart';
import 'design.dart';

/// What the frame rate meter shows: frames per second while the app draws, and the longest
/// frame's work (build on the UI thread or raster, whichever took longer), in the frames since
/// the last reading.
@immutable
class FrameRateReading {
  const FrameRateReading({required this.fps, required this.worst, this.idle = false});

  final double fps;
  final Duration worst;

  /// Nothing has drawn since the last reading: the numbers are the last burst's.
  final bool idle;

  @override
  bool operator ==(Object other) =>
      other is FrameRateReading && other.fps.round() == fps.round() && other.worst == worst && other.idle == idle;

  @override
  int get hashCode => Object.hash(fps.round(), worst, idle);
}

/// Turns the engine's frame timings into readings. The rate is the frames drawn over the time
/// between their vsyncs, counting only frames that follow the previous one without a rest: a
/// frame whose vsync comes more than [idleGap] after the previous frame finished rastering was
/// not waiting on it, so the app had nothing to draw in between (no animation, no input), and
/// that pause is no dropped frame. So the rate is 120 on a 120 Hz display while scrolling
/// smoothly and lower as frames drop, rather than falling whenever the app rests.
class FrameRateCounter {
  /// Longer than one refresh period at 60 Hz, the slowest display the app runs on.
  static const idleGap = Duration(milliseconds: 20);

  int? _lastVsync;
  int? _lastFinish;
  var _span = 0;
  var _intervals = 0;
  var _worst = 0;
  FrameRateReading? _last;

  /// Adds a batch of timings, in the order the frames were drawn.
  void add(List<FrameTiming> timings) {
    for (final timing in timings) {
      final vsync = timing.timestampInMicroseconds(FramePhase.vsyncStart);
      final previousVsync = _lastVsync;
      final previousFinish = _lastFinish;
      _lastVsync = vsync;
      _lastFinish = timing.timestampInMicroseconds(FramePhase.rasterFinish);
      _worst = math.max(_worst, math.max(timing.buildDuration.inMicroseconds, timing.rasterDuration.inMicroseconds));
      if (previousVsync == null || previousFinish == null) continue;
      final interval = vsync - previousVsync;
      if (interval <= 0 || vsync - previousFinish > idleGap.inMicroseconds) continue;
      _span += interval;
      _intervals++;
    }
  }

  /// The reading for the frames added since the last one: the last burst's, marked idle, when
  /// each of them followed a rest; null before anything has drawn.
  FrameRateReading? take() {
    final last = _last;
    final FrameRateReading? reading;
    if (_intervals == 0) {
      reading = last == null ? null : FrameRateReading(fps: last.fps, worst: last.worst, idle: true);
    } else {
      reading = FrameRateReading(fps: _intervals * Duration.microsecondsPerSecond / _span, worst: Duration(microseconds: _worst));
    }
    _span = 0;
    _intervals = 0;
    _worst = 0;
    return _last = reading;
  }
}

/// The app with the [FrameRateMeter] in its bottom leading corner while `flutter.showFrameRate`
/// is on, above every route (sheets included); it rebuilds only when the setting flips.
class FrameRateLayer extends StatefulWidget {
  const FrameRateLayer({super.key, required this.state, required this.child});

  final MailState state;
  final Widget child;

  @override
  State<FrameRateLayer> createState() => _FrameRateLayerState();
}

class _FrameRateLayerState extends State<FrameRateLayer> {
  late bool _shown = widget.state.showFrameRate;

  @override
  void initState() {
    super.initState();
    widget.state.addListener(_follow);
  }

  @override
  void dispose() {
    widget.state.removeListener(_follow);
    super.dispose();
  }

  void _follow() {
    if (widget.state.showFrameRate != _shown) setState(() => _shown = !_shown);
  }

  @override
  Widget build(BuildContext context) => Stack(
    fit: StackFit.expand,
    children: [
      widget.child,
      if (_shown) const Positioned(left: 16, bottom: 16, child: FrameRateMeter()),
    ],
  );
}

/// The current frame rate and the slowest recent frame, in a small capsule over the window
/// (`flutter.showFrameRate`). It costs nothing while not mounted. Mounted, it reads the frame
/// timings the engine reports in batches (about once a second in release builds), updates at
/// most every [refresh], and repaints only itself.
class FrameRateMeter extends StatefulWidget {
  const FrameRateMeter({super.key});

  static const refresh = Duration(milliseconds: 500);

  @override
  State<FrameRateMeter> createState() => _FrameRateMeterState();
}

class _FrameRateMeterState extends State<FrameRateMeter> {
  final _counter = FrameRateCounter();
  final _reading = ValueNotifier<FrameRateReading?>(null);
  final _sinceShown = Stopwatch();
  Timer? _pending;

  @override
  void initState() {
    super.initState();
    SchedulerBinding.instance.addTimingsCallback(_onTimings);
  }

  @override
  void dispose() {
    SchedulerBinding.instance.removeTimingsCallback(_onTimings);
    _pending?.cancel();
    _reading.dispose();
    super.dispose();
  }

  void _onTimings(List<FrameTiming> timings) {
    _counter.add(timings);
    if (_pending != null) return;
    final wait = FrameRateMeter.refresh - _sinceShown.elapsed;
    if (!_sinceShown.isRunning || wait <= Duration.zero) return _show();
    _pending = Timer(wait, _show);
  }

  void _show() {
    _pending = null;
    _sinceShown
      ..reset()
      ..start();
    _reading.value = _counter.take();
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final style = context.text.caption1.copyWith(color: colors.onToast, fontFeatures: const [FontFeature.tabularFigures()]);
    final refreshRate = View.of(context).display.refreshRate;
    return IgnorePointer(
      child: RepaintBoundary(
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
          decoration: ShapeDecoration(color: colors.toast.withValues(alpha: 0.85), shape: const StadiumBorder()),
          child: ValueListenableBuilder(
            valueListenable: _reading,
            builder: (context, reading, _) {
              if (reading == null) return Text('– fps', style: style);
              // Below 90% of the display's rate, or a frame over its budget, shows red.
              final slowRate = refreshRate > 0 && reading.fps < refreshRate * 0.9;
              final slowFrame = refreshRate > 0 && reading.worst.inMicroseconds > Duration.microsecondsPerSecond / refreshRate;
              final ms = reading.worst.inMicroseconds / Duration.microsecondsPerMillisecond;
              return Opacity(
                opacity: reading.idle ? 0.55 : 1,
                child: Text.rich(
                  TextSpan(children: [
                    TextSpan(
                      text: '${reading.fps.round()} fps',
                      style: TextStyle(fontWeight: FontWeight.w600, color: slowRate ? colors.red : null),
                    ),
                    const TextSpan(text: '  ·  max '),
                    TextSpan(text: '${ms.toStringAsFixed(1)} ms', style: TextStyle(color: slowFrame ? colors.red : null)),
                  ]),
                  style: style,
                ),
              );
            },
          ),
        ),
      ),
    );
  }
}
