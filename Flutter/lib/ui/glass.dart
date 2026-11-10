/// The window's surfaces, after Dia's islands. On the Mac the window itself is glass
/// (`NSGlassEffectView`, under the transparent Flutter view: `macos/Runner/MainFlutterWindow.swift`)
/// and the sidebar sits on it; on Windows the window is a color (`MailColors.window`). That is
/// window chrome, the one place the platforms differ: everything drawn on it is the app's own and
/// the same on both (MS-182). The list and the conversation are [Island]s; toolbar groups float
/// on opaque [GlassCapsule]s over the content scrolling under them; the controls that float
/// over a row are [RaisedCapsule]s, a little skeuomorphic.
library;

import 'package:flutter/cupertino.dart' show CupertinoActivityIndicator, CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:flutter/services.dart';
import 'package:macos_ui/macos_ui.dart';

import '../platform.dart';
import 'common.dart';
import 'controls.dart';
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
/// an opaque [MailColors.capsule] face inside a [MailColors.capsuleRim] hairline. Flutter draws
/// it on every platform, in the app's light or dark (MS-177: the system's glass followed the
/// system's appearance, not the app's, and went pale under dark controls). It blurs nothing
/// beneath it: a backdrop blur, even one shared by every capsule, cost 2–3 ms of every frame in
/// the reader's swipe (MS-187).
class GlassCapsule extends StatelessWidget {
  const GlassCapsule({super.key, required this.child});

  /// The capsule's height: a 30 pt control and its padding.
  static const height = 36.0;

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return SizedBox(
      height: height,
      child: DecoratedBox(
        decoration: ShapeDecoration(
          color: colors.capsule,
          shape: StadiumBorder(side: BorderSide(color: colors.capsuleRim, width: Strokes.hairline)),
        ),
        child: Padding(padding: const EdgeInsets.all(3), child: child),
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

/// A toolbar's icon button: a [MailIconButton] at the toolbar's size, with a tooltip.
class ToolButton extends StatelessWidget {
  const ToolButton({super.key, required this.icon, required this.tooltip, required this.onPressed, this.color});

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;
  final Color? color;

  @override
  Widget build(BuildContext context) => MailIconButton(icon: icon, tooltip: tooltip, onPressed: onPressed, color: color);
}

/// A detail pane's toolbar (the open conversation, chat, event or draft; MS-188): its close
/// capsule at the leading edge, the pane's [title] after it when it has one (a chat's name,
/// "New Message", in [TextRoles.paneTitle]), and its action capsules at the trailing edge, with
/// no bar or rule under them. A pane's own title (a subject, an event's) is its content's first
/// line, in [TextRoles.display], and scrolls under the toolbar with it.
class DetailToolbar extends StatelessWidget {
  const DetailToolbar({super.key, required this.leading, this.title, this.actions = const []});

  /// The band at the top of a detail pane that its toolbar floats in.
  static const height = 56.0;

  final Widget leading;
  final Widget? title;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: height,
    child: Padding(
      padding: const EdgeInsets.symmetric(horizontal: Space.s10),
      child: Row(children: [
        leading,
        if (title case final title?) ...[const SizedBox(width: Space.s12), Expanded(child: title)] else const Spacer(),
        for (final (index, action) in actions.indexed) ...[
          if (index > 0 || title != null) const SizedBox(width: Space.s8),
          action,
        ],
      ]),
    ),
  );
}

/// A detail pane: [content] fills it and scrolls under its [toolbar] (a [DetailToolbar]), which
/// floats over the top and, on the Mac, moves the window. With [fade] the content melts into the
/// pane's background under the toolbar, so a title on the toolbar stays readable over it (chat,
/// whose name rides on the toolbar). Mail leaves it off: nothing Flutter draws may cover its
/// WebKit bodies ([Island]).
class DetailPane extends StatelessWidget {
  const DetailPane({super.key, required this.toolbar, required this.content, this.fade = false});

  final Widget toolbar;
  final Widget content;
  final bool fade;

  @override
  Widget build(BuildContext context) {
    final background = context.colors.background;
    return Stack(
      children: [
        Positioned.fill(child: content),
        if (fade)
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            height: DetailToolbar.height + Space.s16,
            child: IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    begin: Alignment.topCenter,
                    end: Alignment.bottomCenter,
                    colors: [background, background, background.withValues(alpha: 0)],
                    stops: const [0, 0.6, 1],
                  ),
                ),
              ),
            ),
          ),
        Positioned(top: 0, left: 0, right: 0, child: WindowDragArea(child: toolbar)),
      ],
    );
  }
}

/// The capsule that leads a detail pane's toolbar: ✕ closes the pane beside the list (two
/// panes), ‹ goes back to the list it covers.
class CloseCapsule extends StatelessWidget {
  const CloseCapsule({super.key, required this.twoPane, required this.onPressed});

  final bool twoPane;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) => GlassCapsule(
    child: ToolButton(
      tooltip: '${twoPane ? 'Close' : 'Back'} (Esc or ${Host.shortcut('W')})',
      icon: twoPane ? CupertinoIcons.xmark : CupertinoIcons.chevron_left,
      onPressed: onPressed,
    ),
  );
}

/// The activity spinner, in the secondary label (or [color]: white on a filled button).
class Spinner extends StatelessWidget {
  const Spinner({super.key, this.radius = 9, this.color});

  final double radius;
  final Color? color;

  @override
  Widget build(BuildContext context) => CupertinoActivityIndicator(radius: radius, color: color ?? context.colors.secondaryLabel);
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
