/// The window's surfaces, after Dia's islands on macOS 26's Liquid Glass. The window itself is
/// glass (`NSGlassEffectView`, under the transparent Flutter view: `macos/Runner/MainFlutterWindow.swift`),
/// the sidebar sits on it, and the list and the conversation are [Island]s. Toolbar groups float
/// on [GlassCapsule]s, real system glass over the content scrolling under them; the controls
/// that float over a row are [RaisedCapsule]s, a little skeuomorphic.
library;

import 'package:flutter/widgets.dart';
import 'package:flutter/services.dart';
import 'package:macos_ui/macos_ui.dart';

import 'design.dart';

/// A rounded island on the window's glass, in the window's background color (or `color`): a
/// hairline rim and a soft shadow lift it.
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
      position: DecorationPosition.foreground,
      decoration: BoxDecoration(
        borderRadius: radius,
        border: Border.all(color: light ? const Color(0x12000000) : const Color(0x17FFFFFF), width: 0.5),
      ),
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: color ?? colors.background,
          borderRadius: radius,
          boxShadow: [
            BoxShadow(color: MacosColors.black.withValues(alpha: light ? 0.05 : 0.3), blurRadius: 1, offset: const Offset(0, 0.5)),
            BoxShadow(color: MacosColors.black.withValues(alpha: light ? 0.07 : 0.35), blurRadius: 18, offset: const Offset(0, 4)),
          ],
        ),
        child: ClipRRect(borderRadius: radius, child: child),
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
          colors: light ? const [MacosColors.white, Color(0xFFF4F4F6)] : const [Color(0xFF424245), Color(0xFF35353A)],
        ),
        shape: StadiumBorder(side: BorderSide(color: light ? MacosColors.black.withValues(alpha: 0.1) : MacosColors.white.withValues(alpha: 0.12), width: 0.5)),
        shadows: [
          BoxShadow(color: MacosColors.black.withValues(alpha: light ? 0.08 : 0.4), blurRadius: 2, offset: const Offset(0, 1)),
          BoxShadow(color: MacosColors.black.withValues(alpha: light ? 0.08 : 0.3), blurRadius: 10, offset: const Offset(0, 4)),
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
    final tint = color ?? colors.secondaryLabel;
    return MacosTooltip(
      message: tooltip,
      child: MacosIconButton(
        icon: MacosIcon(icon, size: 17, color: onPressed == null ? tint.withValues(alpha: 0.35) : tint),
        onPressed: onPressed,
        semanticLabel: tooltip,
        backgroundColor: MacosColors.transparent,
        disabledColor: MacosColors.transparent,
        hoverColor: colors.label.withValues(alpha: 0.07),
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

/// Opens [builder]'s [Sheet] over the window, the window dimmed behind it.
Future<T?> showSheet<T>(BuildContext context, WidgetBuilder builder) => showMacosSheet<T>(context: context, builder: builder);

/// A sheet, as the Mac drops them over a window: a rounded panel in the window's background
/// color with a hairline rim and a deep shadow, an optional [title] over [child] and the
/// [actions] (push buttons, the default last) at its bottom trailing edge. Esc and ⌘W close it;
/// when no field inside takes focus, the sheet itself holds it, so they do from the start.
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
        const SingleActivator(LogicalKeyboardKey.keyW, meta: true): close,
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
                  borderRadius: BorderRadius.circular(Panes.sheetCorner),
                  side: BorderSide(color: colors.separator, width: 0.5),
                ),
                shadows: [
                  BoxShadow(color: MacosColors.black.withValues(alpha: 0.28), blurRadius: 40, offset: const Offset(0, 16)),
                ],
              ),
              child: content,
            ),
          ),
        ),
      ),
    );
  }
}
