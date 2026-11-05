import 'dart:ui' show ImageFilter;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'design.dart';

/// Liquid Glass, as macOS 26 uses it: the navigation sits on a panel of the system's glass
/// (`NSGlassEffectView`, placed under the Flutter view by `macos/Runner/MainFlutterWindow.swift`),
/// and the controls over the content are glass capsules on frosted bars the content scrolls
/// under. Flutter cannot sample native glass, so those bars and capsules are drawn here.
abstract final class Glass {
  /// Blur of what scrolls under a glass bar.
  static const blur = 24.0;

  /// A glass bar's tint over the content it blurs: enough of the surface to keep text legible.
  static Color tint(ColorScheme colors) => colors.surface.withValues(alpha: 0.78);

  /// A capsule's fill: a raised piece of glass, lighter than what it sits on.
  static Color capsule(ColorScheme colors) =>
      colors.brightness == Brightness.light ? Colors.white.withValues(alpha: 0.7) : Colors.white.withValues(alpha: 0.10);

  /// The rim light along a capsule's edge.
  static Color rim(ColorScheme colors) =>
      colors.brightness == Brightness.light ? Colors.black.withValues(alpha: 0.07) : Colors.white.withValues(alpha: 0.16);

  /// The soft shadow that lifts a capsule off the content.
  static List<BoxShadow> lift(ColorScheme colors) => [
    BoxShadow(color: Colors.black.withValues(alpha: colors.brightness == Brightness.light ? 0.08 : 0.3), blurRadius: 10, offset: const Offset(0, 2)),
  ];
}

/// The navigation's glass panel, inset from the window's edges: the window's surface covers
/// everything around it, and the window places the system's glass under its rect (the
/// `mailstream/window` channel's `panel`), so the desktop shows through it, blurred.
class GlassPanel extends StatefulWidget {
  const GlassPanel({super.key, required this.child});

  final Widget child;

  @override
  State<GlassPanel> createState() => _GlassPanelState();
}

class _GlassPanelState extends State<GlassPanel> {
  static const _window = MethodChannel('mailstream/window');

  /// The rect last sent to the window, so an unchanged layout sends nothing.
  Rect? _sent;

  void _place() {
    if (!mounted) return;
    final box = context.findRenderObject();
    if (box is! RenderBox || !box.hasSize) return;
    final rect = (box.localToGlobal(Offset.zero) & box.size).deflate(Panes.inset);
    if (rect == _sent) return;
    _sent = rect;
    _window.invokeMethod<void>('panel', {
      'x': rect.left,
      'y': rect.top,
      'width': rect.width,
      'height': rect.height,
      'radius': Panes.panelCorner,
    });
  }

  @override
  void dispose() {
    _window.invokeMethod<void>('panel');
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    WidgetsBinding.instance.addPostFrameCallback((_) => _place());
    return CustomPaint(
      painter: _Surround(color: context.colors.surface),
      child: Padding(padding: const EdgeInsets.all(Panes.inset), child: widget.child),
    );
  }
}

/// Paints the window's surface around the glass panel, leaving the panel's rounded rect open.
class _Surround extends CustomPainter {
  const _Surround({required this.color});

  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final outer = Offset.zero & size;
    final panel = RRect.fromRectAndRadius(outer.deflate(Panes.inset), const Radius.circular(Panes.panelCorner));
    canvas.drawPath(
      Path()
        ..fillType = PathFillType.evenOdd
        ..addRect(outer)
        ..addRRect(panel),
      Paint()..color = color,
    );
  }

  @override
  bool shouldRepaint(_Surround old) => old.color != color;
}

/// A bar over scrolling content: frosted glass, the content under it blurred and tinted.
class GlassBar extends StatelessWidget {
  const GlassBar({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => ClipRect(
    child: BackdropFilter(
      filter: ImageFilter.blur(sigmaX: Glass.blur, sigmaY: Glass.blur),
      child: ColoredBox(color: Glass.tint(context.colors), child: child),
    ),
  );
}

/// Controls grouped on one glass capsule, as macOS 26 groups toolbar items. [blur] frosts what
/// is under it, for capsules that float over text; capsules already on a [GlassBar] or a plain
/// surface skip it, so scrolling under them stays cheap.
class GlassCapsule extends StatelessWidget {
  const GlassCapsule({super.key, required this.child, this.blur = false});

  final Widget child;
  final bool blur;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final capsule = DecoratedBox(
      decoration: ShapeDecoration(
        color: blur ? Glass.tint(colors) : Glass.capsule(colors),
        shape: StadiumBorder(side: BorderSide(color: Glass.rim(colors), width: 0.5)),
        shadows: blur ? null : Glass.lift(colors),
      ),
      child: Padding(padding: const EdgeInsets.all(3), child: child),
    );
    if (!blur) return capsule;
    return DecoratedBox(
      decoration: ShapeDecoration(shape: const StadiumBorder(), shadows: Glass.lift(colors)),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(100),
        child: BackdropFilter(filter: ImageFilter.blur(sigmaX: Glass.blur, sigmaY: Glass.blur), child: capsule),
      ),
    );
  }
}
