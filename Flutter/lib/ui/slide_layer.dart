import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';

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
  late final _presence = AnimationController(vsync: this, duration: Motion.standard);

  /// Horizontal offset in pixels while swiping, and while it settles back or flies off.
  late final _drag = AnimationController.unbounded(vsync: this);
  Widget? _shown;

  /// Set once a swipe dismissed the layer, which is already off screen: no slide-out follows.
  bool _swipedAway = false;

  // Swipe tracking.
  bool? _horizontal;
  double _velocity = 0;
  Duration? _lastStamp;

  static const _decisionDistance = 6.0;

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
    _horizontal = null;
    _velocity = 0;
    _lastStamp = event.timeStamp;
    _drag.stop();
  }

  void _update(PointerPanZoomUpdateEvent event) {
    final pan = event.pan;
    _horizontal ??= pan.distance >= _decisionDistance ? pan.dx.abs() > pan.dy.abs() * 1.5 : null;
    if (_horizontal != true) return;
    final elapsed = (event.timeStamp - (_lastStamp ?? event.timeStamp)).inMicroseconds;
    if (elapsed > 0) _velocity = event.panDelta.dx / elapsed * 1e6;
    _lastStamp = event.timeStamp;
    _drag.value = pan.dx;
  }

  void _end(double width) {
    if (_horizontal != true) return;
    _horizontal = null;
    final offset = _drag.value;
    final flicked = _velocity.abs() > 900 && _velocity.sign == offset.sign;
    if (offset.abs() > width / 3 || flicked) {
      _swipedAway = true;
      _drag
          .animateTo(offset.sign * width, duration: Motion.brief, curve: houseCurve)
          .whenComplete(widget.onDismiss);
    } else {
      _drag.animateTo(0, duration: Motion.standard, curve: houseCurve);
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
              final enter = houseCurve.transform(_presence.value);
              final x = (1 - enter) * width + _drag.value;
              return Transform.translate(offset: Offset(x, 0), child: child);
            },
            child: DecoratedBox(
              decoration: BoxDecoration(boxShadow: Shadows.page(context.brightness)),
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
