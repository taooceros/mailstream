/// The window's surfaces, after Dia's islands on macOS 26's Liquid Glass. The window itself is
/// glass (`NSGlassEffectView`, under the transparent Flutter view: `macos/Runner/MainFlutterWindow.swift`),
/// the sidebar sits on it, and the list and the conversation are [Island]s. Toolbar groups float
/// on [GlassCapsule]s, real system glass over the content scrolling under them; the controls
/// that float over a row are [RaisedCapsule]s, a little skeuomorphic.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:macos_ui/macos_ui.dart';

import 'design.dart';

/// A rounded white island on the window's glass: a hairline rim and a soft shadow lift it. It is
/// a [Material], so the rows' hover and press highlights paint on it.
class Island extends StatelessWidget {
  const Island({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final light = colors.brightness == Brightness.light;
    const radius = BorderRadius.all(Radius.circular(Panes.corner));
    return DecoratedBox(
      decoration: BoxDecoration(
        borderRadius: radius,
        boxShadow: [
          BoxShadow(color: Colors.black.withValues(alpha: light ? 0.05 : 0.3), blurRadius: 1, offset: const Offset(0, 0.5)),
          BoxShadow(color: Colors.black.withValues(alpha: light ? 0.07 : 0.35), blurRadius: 18, offset: const Offset(0, 4)),
        ],
      ),
      child: Material(
        color: colors.surface,
        clipBehavior: Clip.antiAlias,
        shape: RoundedRectangleBorder(
          borderRadius: radius,
          side: BorderSide(color: light ? Colors.black.withValues(alpha: 0.07) : Colors.white.withValues(alpha: 0.09), width: 0.5),
        ),
        child: child,
      ),
    );
  }
}

/// Controls grouped on a capsule of the system's glass, as macOS 26 groups toolbar items. The
/// glass is an AppKit view under the controls (the `mailstream/glass` platform view), so it
/// refracts whatever Flutter draws beneath it.
class GlassCapsule extends StatelessWidget {
  const GlassCapsule({super.key, required this.child});

  /// The capsule's height: a 30 pt control and its padding.
  static const height = 36.0;

  final Widget child;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: height,
    child: Stack(
      children: [
        const Positioned.fill(
          child: IgnorePointer(
            child: AppKitView(
              viewType: 'mailstream/glass',
              creationParams: {'radius': height / 2},
              creationParamsCodec: StandardMessageCodec(),
            ),
          ),
        ),
        Padding(padding: const EdgeInsets.all(3), child: child),
      ],
    ),
  );
}

/// A raised capsule drawn by Flutter, for controls over a row: lit from above (a top-to-bottom
/// gradient and a bright inner edge) with a soft drop shadow.
class RaisedCapsule extends StatelessWidget {
  const RaisedCapsule({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final light = context.colors.brightness == Brightness.light;
    return DecoratedBox(
      decoration: ShapeDecoration(
        gradient: LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: light ? const [Colors.white, Color(0xFFF4F4F6)] : const [Color(0xFF424245), Color(0xFF35353A)],
        ),
        shape: StadiumBorder(side: BorderSide(color: light ? Colors.black.withValues(alpha: 0.1) : Colors.white.withValues(alpha: 0.12), width: 0.5)),
        shadows: [
          BoxShadow(color: Colors.black.withValues(alpha: light ? 0.08 : 0.4), blurRadius: 2, offset: const Offset(0, 1)),
          BoxShadow(color: Colors.black.withValues(alpha: light ? 0.08 : 0.3), blurRadius: 10, offset: const Offset(0, 4)),
        ],
      ),
      child: Padding(padding: const EdgeInsets.all(3), child: child),
    );
  }
}

/// A toolbar's icon button: macos_ui's, a rounded hover fill, with a tooltip.
class ToolButton extends StatelessWidget {
  const ToolButton({super.key, required this.icon, required this.tooltip, required this.onPressed, this.color});

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final tint = color ?? colors.onSurfaceVariant;
    return MacosTooltip(
      message: tooltip,
      child: MacosIconButton(
        icon: MacosIcon(icon, size: 17, color: onPressed == null ? tint.withValues(alpha: 0.35) : tint),
        onPressed: onPressed,
        semanticLabel: tooltip,
        backgroundColor: Colors.transparent,
        disabledColor: Colors.transparent,
        hoverColor: colors.onSurface.withValues(alpha: 0.07),
        borderRadius: BorderRadius.circular(15),
        padding: EdgeInsets.zero,
        boxConstraints: const BoxConstraints.tightFor(width: 30, height: 30),
      ),
    );
  }
}

/// macos_ui's spinner.
class Spinner extends StatelessWidget {
  const Spinner({super.key, this.radius = 9});

  final double radius;

  @override
  Widget build(BuildContext context) => ProgressCircle(radius: radius);
}
