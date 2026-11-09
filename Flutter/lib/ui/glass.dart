/// The window's surfaces, after Dia's islands. On the Mac the window itself is glass
/// (`NSGlassEffectView`, under the transparent Flutter view: `macos/Runner/MainFlutterWindow.swift`)
/// and the sidebar sits on it; on Windows the window is a color (`MailColors.window`). That is
/// window chrome, the one place the platforms differ: everything drawn on it is the app's own and
/// the same on both (MS-182). The list and the conversation are [Island]s; toolbar groups float
/// on frosted [GlassCapsule]s over the content scrolling under them; the controls that float
/// over a row are [RaisedCapsule]s, a little skeuomorphic.
library;

import 'dart:ui' show ImageFilter;

import 'package:flutter/widgets.dart';
import 'package:flutter/services.dart';
import 'package:macos_ui/macos_ui.dart';

import '../platform.dart';
import 'design.dart';

/// A rounded island on the window's glass, in the window's background color (or `color`): a
/// hairline rim and a soft shadow lift it. The rim is stroked just outside the island, under its
/// content: drawn over the content, its bounds (the whole island) would cover the WebKit bodies,
/// and AppKit would give their clicks to Flutter.
class Island extends StatelessWidget {
  const Island({super.key, required this.child, this.color});

  final Widget child;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final light = colors.brightness == Brightness.light;
    const radius = BorderRadius.all(Radius.circular(Panes.corner));
    return DecoratedBox(
      decoration: BoxDecoration(
        color: color ?? colors.background,
        borderRadius: radius,
        border: Border.all(
          color: light ? const Color(0x12000000) : const Color(0x17FFFFFF),
          width: 0.5,
          strokeAlign: BorderSide.strokeAlignOutside,
        ),
        boxShadow: colors.islandShadow,
      ),
      child: ClipRRect(borderRadius: radius, child: child),
    );
  }
}

/// Controls grouped on a capsule floating over the content, as macOS 26 groups toolbar items:
/// what scrolls under it blurs behind a translucent [MailColors.capsule] face inside a hairline
/// rim. Flutter draws it on every platform, in the app's light or dark (MS-177: the system's
/// glass followed the system's appearance, not the app's, and went pale under dark controls).
/// The capsules share the app's [BackdropGroup] (`main.dart`): the engine reads and blurs the
/// window once for all of them rather than once per capsule.
class GlassCapsule extends StatelessWidget {
  const GlassCapsule({super.key, required this.child});

  /// The capsule's height: a 30 pt control and its padding.
  static const height = 36.0;

  /// How far what lies beneath blurs.
  static const blur = 12.0;

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return SizedBox(
      height: height,
      child: ClipRRect(
        borderRadius: const BorderRadius.all(Radius.circular(height / 2)),
        child: BackdropFilter.grouped(
          filter: ImageFilter.blur(sigmaX: blur, sigmaY: blur),
          child: DecoratedBox(
            decoration: ShapeDecoration(
              color: colors.capsule,
              shape: StadiumBorder(side: BorderSide(color: colors.capsuleRim, width: Strokes.hairline)),
            ),
            child: Padding(padding: const EdgeInsets.all(3), child: child),
          ),
        ),
      ),
    );
  }
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
          colors: light ? const [MacosColors.white, Color(0xFFF4F4F6)] : const [Color(0xFF424245), Color(0xFF35353A)],
        ),
        shape: StadiumBorder(side: BorderSide(color: light ? MacosColors.black.withValues(alpha: 0.1) : MacosColors.white.withValues(alpha: 0.12), width: 0.5)),
        shadows: context.colors.raisedShadow,
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
    final tint = color ?? colors.secondaryLabel;
    return MacosTooltip(
      message: tooltip,
      child: MacosIconButton(
        icon: MacosIcon(icon, size: IconSizes.toolbar, color: onPressed == null ? tint.withValues(alpha: 0.35) : tint),
        onPressed: onPressed,
        semanticLabel: tooltip,
        backgroundColor: MacosColors.transparent,
        disabledColor: MacosColors.transparent,
        hoverColor: colors.label.withValues(alpha: 0.07),
        borderRadius: BorderRadius.circular(GlassCapsule.height / 2),
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

/// Opens [builder]'s [Sheet] over the window, the window dimmed behind it.
Future<T?> showSheet<T>(BuildContext context, WidgetBuilder builder) => showMacosSheet<T>(context: context, builder: builder);

/// A sheet, as the Mac drops them over a window: a rounded panel in the window's background
/// color with a hairline rim and a deep shadow, an optional [title] over [child] and the
/// [actions] (push buttons, the default last) at its bottom trailing edge. Esc and ⌘W (Ctrl+W
/// off the Mac) close it; when no field inside takes focus, the sheet itself holds it, so they do
/// from the start.
class Sheet extends StatefulWidget {
  const Sheet({super.key, required this.width, this.height, this.title, this.actions = const [], required this.child});

  final double width;

  /// Nil sizes the sheet to its content.
  final double? height;
  final String? title;
  final List<Widget> actions;
  final Widget child;

  @override
  State<Sheet> createState() => _SheetState();
}

class _SheetState extends State<Sheet> {
  final _focus = FocusNode(debugLabel: 'Sheet');
  Animation<double>? _entrance;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final entrance = ModalRoute.of(context)?.animation;
    if (entrance == _entrance) return;
    _entrance?.removeStatusListener(_settled);
    _entrance = entrance?..addStatusListener(_settled);
  }

  /// Once the sheet is in, the fields' autofocus has had its turn: nothing focused inside takes
  /// the sheet's own node.
  void _settled(AnimationStatus status) {
    if (status.isCompleted && !_focus.hasFocus) _focus.requestFocus();
  }

  @override
  void dispose() {
    _entrance?.removeStatusListener(_settled);
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final Sheet(:height, :title, :actions, :child) = widget;
    void close() => Navigator.of(context).maybePop();
    final content = Column(
      mainAxisSize: height == null ? MainAxisSize.min : MainAxisSize.max,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (title != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 18, 20, 12),
            child: Text(title, style: context.text.title3.copyWith(color: colors.label, fontWeight: FontWeight.w700)),
          ),
        if (height == null) Flexible(child: child) else Expanded(child: child),
        if (actions.isNotEmpty)
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 12, 20, 18),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                for (final (i, action) in actions.indexed) ...[if (i > 0) const SizedBox(width: 10), action],
              ],
            ),
          ),
      ],
    );
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.escape): close,
        Host.command(LogicalKeyboardKey.keyW): close,
      },
      child: Focus(
        focusNode: _focus,
        child: Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: Container(
              width: widget.width,
              height: height,
              clipBehavior: Clip.antiAlias,
              decoration: ShapeDecoration(
                color: colors.background,
                shape: RoundedRectangleBorder(
                  borderRadius: BorderRadius.circular(Radii.sheet),
                  side: BorderSide(color: colors.separator, width: Strokes.hairline),
                ),
                shadows: colors.sheetShadow,
              ),
              child: content,
            ),
          ),
        ),
      ),
    );
  }
}
