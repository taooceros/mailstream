import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/physics.dart';

import 'design.dart';

/// A page that slides in over the one beneath, which stays mounted and in place: the reader over
/// the list, a bundle over the inbox. A two-finger horizontal swipe on the trackpad drags it off
/// that side and goes back once past a third of its width or flicked.
class SlideLayer extends StatefulWidget {
  const SlideLayer({super.key, required this.child, required this.onDismiss});

  /// Nil hides the layer, sliding the last child out.
  final Widget? child;
  final VoidCallback onDismiss;

  @override
  State<SlideLayer> createState() => _SlideLayerState();
}

class _SlideLayerState extends State<SlideLayer> with TickerProviderStateMixin {
  late final _presence = AnimationController(vsync: this, duration: Motion.enter, reverseDuration: Motion.exit);

  /// Horizontal offset in pixels while swiping, and while it settles back or flies off.
  late final _drag = AnimationController.unbounded(vsync: this);
  Widget? _shown;

  /// Set once a swipe dismissed the layer, which is already off screen: no slide-out follows.
  bool _swipedAway = false;

  final _swipe = HorizontalSwipe();

  @override
  void initState() {
    super.initState();
    _shown = widget.child;
    if (_shown != null) _presence.forward();
  }

  @override
  void didUpdateWidget(SlideLayer old) {
    super.didUpdateWidget(old);
    final child = widget.child;
    if (child != null) {
      final entering = old.child == null;
      _shown = child;
      if (entering) {
        _swipedAway = false;
        _drag.value = 0;
        _presence.forward(from: _presence.value);
      }
    } else if (old.child != null) {
      if (_swipedAway) {
        _presence.value = 0;
        _shown = null;
      } else {
        _presence.reverse().whenComplete(() {
          if (mounted && widget.child == null) setState(() => _shown = null);
        });
      }
    }
  }

  @override
  void dispose() {
    _presence.dispose();
    _drag.dispose();
    super.dispose();
  }

  void _start(PointerPanZoomStartEvent event) {
    _swipe.start(event);
    _drag.stop();
  }

  void _update(PointerPanZoomUpdateEvent event) {
    if (_swipe.update(event)) _drag.value = event.pan.dx;
  }

  void _end(double width) {
    final velocity = _swipe.end();
    if (velocity == null) return;
    final offset = _drag.value;
    if (HorizontalSwipe.dismisses(offset, velocity, width)) {
      _swipedAway = true;
      _drag.animateWith(_settle(offset, offset.sign * width, velocity)).whenComplete(widget.onDismiss);
    } else {
      _drag.animateWith(_settle(offset, 0, velocity));
    }
  }

  @override
  Widget build(BuildContext context) {
    final shown = _shown;
    if (shown == null) return const SizedBox.shrink();
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        return Listener(
          onPointerPanZoomStart: _start,
          onPointerPanZoomUpdate: _update,
          onPointerPanZoomEnd: (_) => _end(width),
          child: AnimatedBuilder(
            animation: Listenable.merge([_presence, _drag]),
            builder: (context, child) {
              final enter = Motion.curve.transform(_presence.value);
              final x = (1 - enter) * width + _drag.value;
              return Transform.translate(offset: Offset(x, 0), child: child);
            },
            child: Material(
              elevation: 2,
              color: context.colors.surface,
              shadowColor: context.colors.shadow,
              child: shown,
            ),
          ),
        );
      },
    );
  }
}

/// Mouse back buttons go back too.
bool isBackButton(PointerDownEvent event) => event.kind == PointerDeviceKind.mouse && event.buttons == kBackMouseButton;

/// Tracks a two-finger horizontal trackpad swipe: decides its axis after a few pixels and measures
/// the fingers' velocity as they let go.
class HorizontalSwipe {
  bool? _horizontal;
  double _velocity = 0;
  Duration? _lastStamp;

  static const _decisionDistance = 6.0;

  void start(PointerPanZoomStartEvent event) {
    _horizontal = null;
    _velocity = 0;
    _lastStamp = event.timeStamp;
  }

  /// Whether the swipe is horizontal, so the caller should follow `event.pan.dx`.
  bool update(PointerPanZoomUpdateEvent event) {
    final pan = event.pan;
    _horizontal ??= pan.distance >= _decisionDistance ? pan.dx.abs() > pan.dy.abs() * 1.5 : null;
    if (_horizontal != true) return false;
    final elapsed = (event.timeStamp - (_lastStamp ?? event.timeStamp)).inMicroseconds;
    if (elapsed > 0) _velocity = event.panDelta.dx / elapsed * 1e6;
    _lastStamp = event.timeStamp;
    return true;
  }

  /// The release velocity in pixels per second, or null when the swipe was not horizontal.
  double? end() {
    if (_horizontal != true) return null;
    _horizontal = null;
    return _velocity;
  }

  /// Past a third of the page, or flicked that way.
  static bool dismisses(double offset, double velocity, double width) =>
      offset.abs() > width / 3 || (velocity.abs() > 900 && velocity.sign == offset.sign);
}

/// Springs a swiped value from [from] to [to], starting at the fingers' [velocity]; it rests
/// within half a pixel of [to], so a dismissal hands over with the page off screen. [pixels] is
/// how many pixels one unit of the value moves the page.
Simulation _settle(double from, double to, double velocity, {double pixels = 1}) => SpringSimulation(
  Motion.spring,
  from,
  to,
  velocity,
  tolerance: Tolerance(distance: 0.5 / pixels, velocity: 20 / pixels),
);

/// The list-detail split of wide windows: the list beside the opened page, which slides in from
/// the trailing edge as the list narrows. A two-finger swipe to the right on the page drags it
/// out, the list widening behind it, and closes it once past a third of its width or flicked.
class SplitDetail extends StatefulWidget {
  const SplitDetail({
    super.key,
    required this.list,
    required this.detail,
    required this.listWidth,
    required this.onDismiss,
  });

  final Widget list;

  /// Null closes the split; the page last shown slides out.
  final Widget? detail;
  final double listWidth;
  final VoidCallback onDismiss;

  @override
  State<SplitDetail> createState() => _SplitDetailState();
}

class _SplitDetailState extends State<SplitDetail> with SingleTickerProviderStateMixin {
  /// How far the split is open: 0 the list alone, 1 the page fully in. Driven by the open/close
  /// animations and directly by the fingers while swiping.
  late final _split = AnimationController.unbounded(vsync: this);
  final _swipe = HorizontalSwipe();
  Widget? _shown;

  /// Set once a swipe closed the split, which is already springing shut: closing follows on its own.
  bool _swipedAway = false;

  @override
  void initState() {
    super.initState();
    _shown = widget.detail;
    if (_shown != null) _split.value = 1;
  }

  @override
  void didUpdateWidget(SplitDetail old) {
    super.didUpdateWidget(old);
    final detail = widget.detail;
    if (detail != null) {
      _shown = detail;
      if (old.detail == null) {
        _swipedAway = false;
        _split.animateTo(1, duration: Motion.enter, curve: Motion.curve);
      }
    } else if (old.detail != null && !_swipedAway) {
      _split.animateTo(0, duration: Motion.exit, curve: Easing.emphasizedAccelerate).whenComplete(_hide);
    }
  }

  void _hide() {
    if (mounted && widget.detail == null) setState(() => _shown = null);
  }

  @override
  void dispose() {
    _split.dispose();
    super.dispose();
  }

  void _start(PointerPanZoomStartEvent event) {
    if (widget.detail == null) return;
    _swipe.start(event);
    _split.stop();
  }

  void _update(PointerPanZoomUpdateEvent event, double travel) {
    if (widget.detail == null || !_swipe.update(event)) return;
    // Only rightward drags close; leftward ones resist, so the page cannot be pulled past open.
    final dx = event.pan.dx;
    _split.value = 1 - (dx >= 0 ? dx : dx / 4) / travel;
  }

  void _end(double travel) {
    final velocity = _swipe.end();
    if (velocity == null || widget.detail == null) return;
    final offset = (1 - _split.value) * travel;
    final splitVelocity = -velocity / travel;
    if (offset > 0 && HorizontalSwipe.dismisses(offset, velocity, travel)) {
      _swipedAway = true;
      _split.animateWith(_settle(_split.value, 0, splitVelocity, pixels: travel)).whenComplete(() {
        widget.onDismiss();
        _hide();
      });
    } else {
      _split.animateWith(_settle(_split.value, 1, splitVelocity, pixels: travel));
    }
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final full = constraints.maxWidth;
        final detailWidth = full - widget.listWidth - Panes.spacer;
        final travel = full - widget.listWidth;
        return AnimatedBuilder(
          animation: _split,
          builder: (context, _) {
            final split = _split.value.clamp(0.0, 1.0);
            final list = full + (widget.listWidth - full) * split;
            final shown = _shown;
            return ClipRect(
              child: Stack(
                children: [
                  Positioned(left: 0, top: 0, bottom: 0, width: list, child: widget.list),
                  // The page keeps its final width, so its text does not reflow as it slides.
                  if (split > 0 && shown != null)
                    Positioned(
                      left: list + Panes.spacer,
                      top: 0,
                      bottom: 0,
                      width: detailWidth,
                      child: Listener(
                        onPointerPanZoomStart: _start,
                        onPointerPanZoomUpdate: (event) => _update(event, travel),
                        onPointerPanZoomEnd: (_) => _end(travel),
                        child: shown,
                      ),
                    ),
                ],
              ),
            );
          },
        );
      },
    );
  }
}
