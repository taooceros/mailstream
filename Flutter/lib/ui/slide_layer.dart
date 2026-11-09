import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/physics.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

import 'design.dart';

/// A page that slides in over the one beneath, which stays mounted and in place: the reader over
/// the list, a bundle over the inbox. A two-finger horizontal swipe on the trackpad drags it off
/// that side and goes back once past a third of its width or flicked.
class SlideLayer extends StatefulWidget {
  const SlideLayer({super.key, required this.pageId, required this.child, required this.onDismiss});

  /// Stable identity of the open page, even when its widget rebuilds.
  final Object? pageId;

  /// Nil hides the layer, sliding the last child out.
  final Widget? child;
  final VoidCallback onDismiss;

  @override
  State<SlideLayer> createState() => _SlideLayerState();
}

class _SlideLayerState extends State<SlideLayer> with TickerProviderStateMixin {
  late final _presence = AnimationController(vsync: this, duration: Motion.enter);

  /// Horizontal offset in pixels while swiping, while it settles back or flies off, and while the
  /// layer slides out.
  late final _drag = AnimationController.unbounded(vsync: this);
  Widget? _shown;
  late final _ready = SwipeReadiness(this);

  /// Set once a swipe dismissed the layer, which is already off screen: no slide-out follows.
  bool _swipedAway = false;

  final _swipe = HorizontalSwipe();

  /// Captured only once the gesture is horizontal, so scrolling never interrupts a spring.
  double? _dragOrigin;

  /// The layer's width at the last layout, which closing slides it by.
  double _width = 0;

  @override
  void initState() {
    super.initState();
    _shown = widget.child;
    if (_shown != null) _presence.forward();
    _ready.shown = _shown != null;
  }

  @override
  void didUpdateWidget(SlideLayer old) {
    super.didUpdateWidget(old);
    final child = widget.child;
    if (child != null) {
      final entering = old.child == null || (_swipedAway && old.pageId != widget.pageId);
      if (entering) {
        _swipedAway = false;
        if (_shown == null) {
          _drag.value = 0;
          _presence.forward(from: 0);
        } else {
          // Reopened while still sliding out: it springs back from where it is.
          _drag.animateWith(settleSwipe(_drag.value, 0, _drag.velocity));
        }
      }
      _shown = child;
    } else if (old.child != null) {
      if (_swipedAway) {
        _presence.value = 0;
        _shown = null;
      } else {
        // Slides out on the spring a swipe settles on, from wherever the entrance has got to, so
        // it moves at once and glides out instead of creeping off first.
        _drag.value += (1 - Motion.curve.transform(_presence.value)) * _width;
        _presence.value = 1;
        _drag.animateWith(settleSwipe(_drag.value, _width, 0)).whenComplete(() {
          if (mounted && widget.child == null) setState(() => _shown = null);
          if (mounted) _ready.shown = _shown != null;
        });
      }
    }
    _ready.shown = _shown != null;
  }

  @override
  void dispose() {
    _ready.dispose();
    _presence.dispose();
    _drag.dispose();
    super.dispose();
  }

  void _start(PointerPanZoomStartEvent event) {
    _swipe.start(event);
    _dragOrigin = null;
    _ready.fingersDown();
  }

  void _update(PointerPanZoomUpdateEvent event) {
    if (widget.child == null || !_swipe.update(event)) return;
    if (_dragOrigin == null) {
      _ready.swipeTookOver();
      _drag.stop();
      _presence.stop();
      _dragOrigin = _drag.value + (1 - Motion.curve.transform(_presence.value)) * _width;
      _presence.value = 1;
      _swipedAway = false;
    }
    _drag.value = _dragOrigin! + event.pan.dx;
  }

  void _end(double width) {
    _ready.swipeTookOver();
    final velocity = _swipe.end();
    if (velocity == null) return;
    final offset = _drag.value;
    if (HorizontalSwipe.dismisses(offset, velocity, width)) {
      _swipedAway = true;
      _drag.animateWith(settleSwipe(offset, offset.sign * width, velocity)).whenComplete(widget.onDismiss);
    } else {
      _drag.animateWith(settleSwipe(offset, 0, velocity));
    }
  }

  @override
  Widget build(BuildContext context) {
    final shown = _shown;
    if (shown == null) return const SizedBox.shrink();
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = _width = constraints.maxWidth;
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
            child: shown,
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
/// how many pixels one unit of the value moves the page. The calendar's pages settle on it too.
Simulation settleSwipe(double from, double to, double velocity, {double pixels = 1}) => SpringSimulation(
  Motion.spring,
  from,
  to,
  velocity,
  tolerance: Tolerance(distance: 0.5 / pixels, velocity: 20 / pixels),
);

/// Keeps the window ready to draw a swipe at the display's full rate while a page it can swipe
/// away is shown. After a rest the first frames of an animation take two to four times their
/// usual raster time: the macOS embedder drops its cached drawing surfaces after a second without
/// a frame, so the next frames each make window-sized ones again, and the first frame after any
/// pause runs slow as well. A swipe after a few seconds of reading dropped its first frames that
/// way, every time (MS-132). So while a page is [shown] a frame is drawn every [keepAlive], under
/// that second, and frames run from the moment fingers land on the trackpad (macOS starts the pan
/// then, before they move) until the swipe takes over or [warmUp] passes: the slow ones are drawn
/// while the page is still.
class SwipeReadiness {
  SwipeReadiness(TickerProvider vsync) {
    _warm = vsync.createTicker((elapsed) {
      if (elapsed >= warmUp) _warm.stop();
    });
  }

  static const keepAlive = Duration(milliseconds: 800);
  static const warmUp = Duration(milliseconds: 250);

  late final Ticker _warm;
  Timer? _keep;
  bool _shown = false;

  set shown(bool shown) {
    _shown = shown;
    if (shown) {
      _keep ??= Timer.periodic(keepAlive, (_) => SchedulerBinding.instance.scheduleFrame());
    } else {
      _keep?.cancel();
      _keep = null;
      _warm.stop();
    }
  }

  void fingersDown() {
    if (!_shown) return;
    _warm
      ..stop()
      ..start();
  }

  void swipeTookOver() => _warm.stop();

  void dispose() {
    _keep?.cancel();
    _warm.dispose();
  }
}

/// The list-detail split of wide windows: the list beside the opened page, which slides in from
/// the trailing edge as the list narrows. A two-finger swipe to the right on the page drags it
/// out, the list widening behind it, and closes it once past a third of its width or flicked.
/// With [onResize], the gap between them drags the list's width and a double-click on it calls
/// [onResetWidth].
class SplitDetail extends StatefulWidget {
  const SplitDetail({
    super.key,
    required this.list,
    required this.detail,
    required this.pageId,
    required this.listWidth,
    required this.onDismiss,
    this.onResize,
    this.onResetWidth,
  });

  /// The narrowest list, and the narrowest page a list may squeeze beside it.
  static const minList = 300.0;
  static const minDetail = 420.0;

  /// [width] of a list within a split [room] wide: at least [minList], and no wider than leaves
  /// the page [minDetail].
  static double fit(double width, double room) => width.clamp(minList, math.max(minList, room - Panes.gap - minDetail));

  final Widget list;

  /// Null closes the split; the page last shown slides out.
  final Widget? detail;

  /// Stable identity lets a new page interrupt an old page's pending dismissal.
  final Object? pageId;

  /// The width the list asks for, fitted to the room the split has.
  final double listWidth;
  final VoidCallback onDismiss;

  /// Called with the list's width once a drag of the gap ends; the owner keeps it and passes it
  /// back as [listWidth]. Until then the split keeps showing the dragged width.
  final ValueChanged<double>? onResize;
  final VoidCallback? onResetWidth;

  @override
  State<SplitDetail> createState() => _SplitDetailState();
}

class _SplitDetailState extends State<SplitDetail> with TickerProviderStateMixin {
  /// How far the split is open: 0 the list alone, 1 the page fully in. Driven by the open/close
  /// animations and directly by the fingers while swiping.
  late final _split = AnimationController.unbounded(vsync: this);
  final _swipe = HorizontalSwipe();
  double? _splitOrigin;
  Widget? _shown;
  late final _ready = SwipeReadiness(this);

  /// Set once a swipe closed the split, which is already springing shut: closing follows on its own.
  bool _swipedAway = false;

  /// How far the page travels between open and closed, at the last layout.
  double _travel = 1;

  /// The list's width from a drag of the gap, until the owner passes back a new [listWidth]. Only
  /// the split's own layout follows it, so a drag rebuilds nothing above or inside the panes.
  final _dragged = ValueNotifier<double?>(null);
  bool _dragging = false;
  double _dragFrom = 0;
  double _dragBy = 0;

  /// The split's width and the list's width, at the last layout.
  double _room = 0;
  double _listWidth = 0;

  @override
  void initState() {
    super.initState();
    _shown = widget.detail;
    if (_shown != null) _split.value = 1;
    _ready.shown = _shown != null;
  }

  @override
  void didUpdateWidget(SplitDetail old) {
    super.didUpdateWidget(old);
    // The owner kept the dragged width (or its window changed size): its width rules again.
    if (!_dragging && widget.listWidth != old.listWidth) _dragged.value = null;
    final detail = widget.detail;
    if (detail != null) {
      _shown = detail;
      if (old.detail == null || (_swipedAway && old.pageId != widget.pageId)) {
        _swipedAway = false;
        _split.animateTo(1, duration: Motion.enter, curve: Motion.curve);
      }
    } else if (old.detail != null) {
      if (_swipedAway) {
        // The owner has now cleared the detail; remove its native views in this rebuild.
        _shown = null;
        _split.value = 0;
      } else {
        // Closes on the spring a swipe settles on, carrying whatever speed the split has.
        _split.animateWith(settleSwipe(_split.value, 0, _split.velocity, pixels: _travel)).whenComplete(_hide);
      }
    }
    _ready.shown = _shown != null;
  }

  void _hide() {
    if (mounted && widget.detail == null) setState(() => _shown = null);
    if (mounted) _ready.shown = _shown != null;
  }

  @override
  void dispose() {
    _ready.dispose();
    _split.dispose();
    _dragged.dispose();
    super.dispose();
  }

  void _resizeStart() {
    _dragging = true;
    _dragFrom = _listWidth;
    _dragBy = 0;
  }

  // Follows the pointer from where the drag began, so a drag past a limit and back resumes under it.
  void _resize(double dx) {
    _dragBy += dx;
    _dragged.value = SplitDetail.fit(_dragFrom + _dragBy, _room);
  }

  void _resizeEnd() {
    _dragging = false;
    final width = _dragged.value;
    if (width != null) widget.onResize?.call(width);
  }

  void _resetWidth() {
    _dragged.value = null;
    widget.onResetWidth?.call();
  }

  void _start(PointerPanZoomStartEvent event) {
    if (widget.detail == null) return;
    _swipe.start(event);
    _splitOrigin = null;
    _ready.fingersDown();
  }

  void _update(PointerPanZoomUpdateEvent event, double travel) {
    if (widget.detail == null || !_swipe.update(event)) return;
    if (_splitOrigin == null) {
      _ready.swipeTookOver();
      _split.stop();
      _splitOrigin = _split.value.clamp(0.0, 1.0);
      _swipedAway = false;
    }
    // Moving back toward open follows the fingers too; only pulling past open resists.
    final offset = (1 - _splitOrigin!) * travel + event.pan.dx;
    _split.value = 1 - (offset >= 0 ? offset : offset / 4) / travel;
  }

  void _end(double travel) {
    _ready.swipeTookOver();
    final velocity = _swipe.end();
    if (velocity == null || widget.detail == null) return;
    final offset = (1 - _split.value) * travel;
    final splitVelocity = -velocity / travel;
    if (offset > 0 && HorizontalSwipe.dismisses(offset, velocity, travel)) {
      _swipedAway = true;
      _split.animateWith(settleSwipe(_split.value, 0, splitVelocity, pixels: travel)).whenComplete(widget.onDismiss);
    } else {
      _split.animateWith(settleSwipe(_split.value, 1, splitVelocity, pixels: travel));
    }
  }

  @override
  Widget build(BuildContext context) {
    final resizable = widget.onResize != null;
    return LayoutBuilder(
      builder: (context, constraints) {
        final full = _room = constraints.maxWidth;
        final gap = resizable
            ? ResizeGap(onDragStart: _resizeStart, onDrag: _resize, onDragEnd: _resizeEnd, onDoubleTap: _resetWidth)
            : null;
        return AnimatedBuilder(
          animation: Listenable.merge([_split, _dragged]),
          builder: (context, _) {
            final listWidth = _listWidth = SplitDetail.fit(_dragged.value ?? widget.listWidth, full);
            final detailWidth = full - listWidth - Panes.gap;
            _travel = full - listWidth;
            final split = _split.value.clamp(0.0, 1.0);
            final list = full + (listWidth - full) * split;
            final shown = _shown;
            // Unclipped: the islands' shadows reach into the gaps around them, and the page slides
            // in from the window's edge.
            return Stack(
              clipBehavior: Clip.none,
              children: [
                Positioned(left: 0, top: 0, bottom: 0, width: list, child: widget.list),
                // The page keeps its final width, so its text does not reflow as it slides.
                if (split > 0 && shown != null)
                  Positioned(
                    left: list + Panes.gap,
                    top: 0,
                    bottom: 0,
                    width: detailWidth,
                    child: Listener(
                      onPointerPanZoomStart: _start,
                      onPointerPanZoomUpdate: (event) => _update(event, _travel),
                      onPointerPanZoomEnd: (_) => _end(_travel),
                      child: shown,
                    ),
                  ),
                // Only once the page is in: a spring settles within a hair of fully open.
                if (gap != null && split > 0.99 && shown != null && widget.detail != null)
                  Positioned(left: list, top: 0, bottom: 0, width: Panes.gap, child: gap),
              ],
            );
          },
        );
      },
    );
  }
}

/// The gap between two islands that drags horizontally to resize one of them: a resize cursor,
/// and a pill handle on hover and while dragging.
class ResizeGap extends StatefulWidget {
  const ResizeGap({super.key, required this.onDrag, this.onDragStart, this.onDragEnd, this.onDoubleTap});

  final ValueChanged<double> onDrag;
  final VoidCallback? onDragStart;
  final VoidCallback? onDragEnd;
  final VoidCallback? onDoubleTap;

  @override
  State<ResizeGap> createState() => _ResizeGapState();
}

class _ResizeGapState extends State<ResizeGap> {
  bool _hovered = false;
  bool _dragging = false;

  void _end() {
    setState(() => _dragging = false);
    widget.onDragEnd?.call();
  }

  @override
  Widget build(BuildContext context) {
    final active = _hovered || _dragging;
    return MouseRegion(
      cursor: SystemMouseCursors.resizeLeftRight,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        // From the press, so the edge stays under the pointer instead of trailing it by the slop.
        dragStartBehavior: DragStartBehavior.down,
        onHorizontalDragStart: (_) {
          setState(() => _dragging = true);
          widget.onDragStart?.call();
        },
        onHorizontalDragUpdate: (details) => widget.onDrag(details.delta.dx),
        onHorizontalDragEnd: (_) => _end(),
        onHorizontalDragCancel: _end,
        onDoubleTap: widget.onDoubleTap,
        child: SizedBox(
          width: Panes.gap,
          child: Center(
            child: AnimatedOpacity(
              duration: Motion.quick,
              opacity: active ? 1 : 0,
              child: Container(
                width: 3,
                height: 36,
                decoration: BoxDecoration(
                  color: context.colors.border,
                  borderRadius: BorderRadius.circular(1.5),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
