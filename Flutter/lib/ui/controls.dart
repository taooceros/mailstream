/// The app's own controls (MS-193), drawn from the style tokens (`design.dart`) and the same on
/// the Mac and on Windows: buttons ([MailButton], [MailIconButton]), a segmented control
/// ([MailSegmented]), text fields ([MailTextField], [MailSearchField]), a menu button
/// ([MailMenuButton]), a date button ([MailDateButton]), a switch and a checkbox ([MailSwitch],
/// [MailCheckbox]), a notice ([MailNotice], MS-191) and a confirmation sheet ([confirmAction]).
/// They share one look: [Radii.control] corners, a quiet [MailColors.fill] face inside a
/// hairline for secondary controls, the accent for the primary one, the hover and pressed
/// washes, and a ring in the accent while the keyboard focuses them. Text editing itself is
/// macos_ui's `MacosTextField`, which Flutter draws on both platforms; [MailTextField] gives it
/// the app's frame.
library;

import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:intl/intl.dart';
import 'package:macos_ui/macos_ui.dart';

import 'design.dart';
import 'glass.dart' show showSheet, Sheet, Spinner;

/// A control's height: [small] inside rows and cards, [regular] in forms and bars, [large] for a
/// sheet's or a composer's actions.
enum MailControlSize {
  small(22, 8, 12),
  regular(28, 12, 13),
  large(32, 16, 13);

  const MailControlSize(this.height, this.padding, this.fontSize);

  final double height;
  final double padding;
  final double fontSize;
}

/// What a button does: [primary] is the one action a place leads to (the accent), [secondary]
/// the others, [destructive] one that deletes or discards, [plain] a quiet text action.
enum MailButtonKind { primary, secondary, destructive, plain }

/// The focus ring around a control the keyboard is on.
List<BoxShadow> _focusRing(MailColors colors) => [BoxShadow(color: colors.accent.withValues(alpha: 0.45), spreadRadius: 2)];

/// A focusable, pressable surface: Enter and Space press it too. [builder] draws it from whether
/// the pointer is over it, holds it down, or the keyboard focuses it.
class _Pressable extends StatefulWidget {
  const _Pressable({required this.onPressed, required this.builder, this.semanticLabel, this.autofocus = false});

  final VoidCallback? onPressed;
  final Widget Function(BuildContext context, ({bool hovered, bool pressed, bool focused}) state) builder;
  final String? semanticLabel;
  final bool autofocus;

  @override
  State<_Pressable> createState() => _PressableState();
}

class _PressableState extends State<_Pressable> {
  bool _hovered = false;
  bool _pressed = false;
  bool _focused = false;

  @override
  Widget build(BuildContext context) {
    final enabled = widget.onPressed != null;
    return Semantics(
      button: true,
      enabled: enabled,
      label: widget.semanticLabel,
      child: FocusableActionDetector(
        enabled: enabled,
        autofocus: widget.autofocus,
        mouseCursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onShowHoverHighlight: (on) => setState(() => _hovered = on),
        onShowFocusHighlight: (on) => setState(() => _focused = on),
        actions: {ActivateIntent: CallbackAction<ActivateIntent>(onInvoke: (_) => widget.onPressed?.call())},
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapUp: enabled ? (_) => setState(() => _pressed = false) : null,
          onTapCancel: enabled ? () => setState(() => _pressed = false) : null,
          onTap: widget.onPressed,
          child: widget.builder(context, (hovered: enabled && _hovered, pressed: enabled && _pressed, focused: enabled && _focused)),
        ),
      ),
    );
  }
}

/// A button: [label] after an optional [icon], or a spinner while [busy]. Dimmed when
/// [onPressed] is null.
class MailButton extends StatelessWidget {
  const MailButton({
    super.key,
    required this.label,
    required this.onPressed,
    this.kind = MailButtonKind.secondary,
    this.size = MailControlSize.regular,
    this.icon,
    this.busy = false,
    this.autofocus = false,
  });

  final String label;
  final VoidCallback? onPressed;
  final MailButtonKind kind;
  final MailControlSize size;
  final IconData? icon;
  final bool busy;
  final bool autofocus;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final filled = kind == MailButtonKind.primary || kind == MailButtonKind.destructive;
    final face = switch (kind) {
      MailButtonKind.primary => colors.accent,
      MailButtonKind.destructive => colors.red,
      MailButtonKind.secondary => colors.fill,
      MailButtonKind.plain => const Color(0x00000000),
    };
    final ink = switch (kind) {
      MailButtonKind.primary || MailButtonKind.destructive => const Color(0xFFFFFFFF),
      MailButtonKind.secondary => colors.label,
      MailButtonKind.plain => colors.accent,
    };
    final style = context.roles.body.copyWith(fontSize: size.fontSize, fontWeight: filled ? FontWeight.w600 : FontWeight.w500, color: ink, height: 1);
    return _Pressable(
      onPressed: busy ? null : onPressed,
      autofocus: autofocus,
      semanticLabel: label,
      builder: (context, state) {
        // Filled faces darken (lighten in dark mode) under the pointer; quiet ones take the wash.
        final wash = state.pressed ? colors.pressed : (state.hovered ? colors.hover : null);
        final shade = filled ? (colors.brightness == Brightness.light ? const Color(0xFF000000) : const Color(0xFFFFFFFF)) : null;
        final color = wash == null
            ? face
            : filled
            ? Color.lerp(face, shade, state.pressed ? 0.16 : 0.08)!
            : Color.alphaBlend(wash, face);
        return Opacity(
          opacity: onPressed == null && !busy ? 0.4 : 1,
          child: AnimatedContainer(
            duration: Motion.quick,
            height: size.height,
            padding: EdgeInsets.symmetric(horizontal: size.padding),
            decoration: BoxDecoration(
              color: color,
              borderRadius: BorderRadius.circular(Radii.control),
              border: kind == MailButtonKind.secondary ? Border.all(color: colors.cardBorder, width: Strokes.hairline) : null,
              boxShadow: state.focused ? _focusRing(colors) : null,
            ),
            child: Row(mainAxisSize: MainAxisSize.min, children: [
              if (busy) ...[Spinner(radius: size == MailControlSize.small ? 5 : 6, color: ink), const SizedBox(width: Space.s6)]
              else if (icon != null) ...[
                Icon(icon, size: size == MailControlSize.small ? IconSizes.inline : 14, color: ink),
                SizedBox(width: size == MailControlSize.small ? Space.s4 : Space.s6),
              ],
              Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: style),
            ]),
          ),
        );
      },
    );
  }
}

/// A round icon button: the glyph in [color] (the secondary label), a wash under the pointer.
/// [tooltip] names it.
class MailIconButton extends StatelessWidget {
  const MailIconButton({
    super.key,
    required this.icon,
    required this.onPressed,
    this.tooltip,
    this.semanticLabel,
    this.size = 30,
    this.iconSize = IconSizes.toolbar,
    this.color,
  });

  final IconData icon;
  final VoidCallback? onPressed;
  final String? tooltip;
  final String? semanticLabel;

  /// The button's diameter.
  final double size;
  final double iconSize;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final tint = color ?? colors.secondaryLabel;
    final button = _Pressable(
      onPressed: onPressed,
      semanticLabel: semanticLabel ?? tooltip,
      builder: (context, state) => AnimatedContainer(
        duration: Motion.quick,
        width: size,
        height: size,
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: state.pressed ? colors.pressed : (state.hovered ? colors.hover : colors.hover.withValues(alpha: 0)),
          shape: BoxShape.circle,
          boxShadow: state.focused ? _focusRing(colors) : null,
        ),
        child: Icon(icon, size: iconSize, color: onPressed == null ? tint.withValues(alpha: 0.35) : tint),
      ),
    );
    return tooltip == null ? button : MacosTooltip(message: tooltip!, child: button);
  }
}

/// One segment of a [MailSegmented]: its [label], an optional [leading] glyph (a check, a
/// spinner), the [tint] its label takes while selected (the label color when null), a
/// [tooltip].
class MailSegment<T> {
  const MailSegment(this.value, this.label, {this.leading, this.tint, this.tooltip, this.enabled = true});

  final T value;
  final String label;
  final Widget? leading;
  final Color? tint;
  final String? tooltip;
  final bool enabled;
}

/// Segments of equal width side by side in a sunken track, the [selected] one raised on a card
/// that slides to the segment picked: the calendar's views, the answers to an invitation.
/// [compact] for rows. A segment presses only when [onChanged] is set and it is enabled.
class MailSegmented<T> extends StatelessWidget {
  const MailSegmented({super.key, required this.segments, required this.selected, required this.onChanged, this.compact = false});

  final List<MailSegment<T>> segments;
  final T? selected;
  final ValueChanged<T>? onChanged;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final light = colors.brightness == Brightness.light;
    final height = compact ? 20.0 : 24.0;
    Widget segment(MailSegment<T> s) {
      final chosen = s.value == selected;
      final ink = chosen ? (s.tint ?? colors.label) : colors.secondaryLabel;
      final press = onChanged != null && s.enabled && !chosen ? () => onChanged!(s.value) : null;
      final face = _Pressable(
        onPressed: press,
        semanticLabel: s.label,
        builder: (context, state) => AnimatedContainer(
          duration: Motion.quick,
          height: height,
          padding: EdgeInsets.symmetric(horizontal: compact ? Space.s8 : Space.s12),
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: !chosen && state.hovered ? colors.hover : colors.hover.withValues(alpha: 0),
            borderRadius: BorderRadius.circular(Radii.control - 2),
            boxShadow: state.focused ? _focusRing(colors) : null,
          ),
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            if (s.leading != null) ...[s.leading!, const SizedBox(width: Space.s4)],
            AnimatedDefaultTextStyle(
              duration: Motion.short,
              curve: Motion.curve,
              style: context.roles.body.copyWith(
                fontSize: compact ? 12 : 13,
                height: 1,
                color: ink,
                fontWeight: chosen ? FontWeight.w600 : FontWeight.w500,
              ),
              child: Text(s.label, maxLines: 1),
            ),
          ]),
        ),
      );
      return s.tooltip == null ? face : MacosTooltip(message: s.tooltip!, child: face);
    }

    final index = segments.indexWhere((s) => s.value == selected);
    return Container(
      padding: const EdgeInsets.all(Space.s2),
      decoration: BoxDecoration(
        color: colors.strongFill.withValues(alpha: light ? 0.7 : 0.5),
        borderRadius: BorderRadius.circular(Radii.control),
      ),
      child: IntrinsicWidth(
        child: Stack(children: [
          if (index >= 0)
            Positioned.fill(
              child: AnimatedAlign(
                key: const ValueKey('segmentIndicator'),
                duration: Motion.short,
                curve: Motion.curve,
                alignment: Alignment(segments.length == 1 ? 0 : index * 2 / (segments.length - 1) - 1, 0),
                child: FractionallySizedBox(
                  widthFactor: 1 / segments.length,
                  heightFactor: 1,
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      color: colors.raisedCard,
                      borderRadius: BorderRadius.circular(Radii.control - 2),
                      border: Border.all(color: colors.cardBorder, width: Strokes.hairline),
                      boxShadow: colors.cardShadow,
                    ),
                  ),
                ),
              ),
            ),
          Row(children: [for (final s in segments) Expanded(child: segment(s))]),
        ]),
      ),
    );
  }
}

/// A text field in the app's frame: the surface color inside a hairline, a 1pt accent edge and a
/// soft ring while focused; [capsule] rounds it fully (the chat composer), [bare] draws no frame
/// (fields that sit in a form's lines, as the composer's). The placeholder takes the secondary
/// label, readable in both modes.
class MailTextField extends StatefulWidget {
  const MailTextField({
    super.key,
    this.controller,
    this.focusNode,
    this.placeholder,
    this.style,
    this.enabled = true,
    this.autofocus = false,
    this.obscureText = false,
    this.minLines,
    this.maxLines = 1,
    this.expands = false,
    this.keyboardType,
    this.textInputAction,
    this.textAlign = TextAlign.start,
    this.textAlignVertical,
    this.inputFormatters,
    this.onChanged,
    this.onSubmitted,
    this.prefix,
    this.suffix,
    this.padding,
    this.capsule = false,
    this.bare = false,
  });

  final TextEditingController? controller;
  final FocusNode? focusNode;
  final String? placeholder;

  /// The text's style; the body role when null.
  final TextStyle? style;
  final bool enabled;
  final bool autofocus;
  final bool obscureText;
  final int? minLines;
  final int? maxLines;
  final bool expands;
  final TextInputType? keyboardType;
  final TextInputAction? textInputAction;
  final TextAlign textAlign;
  final TextAlignVertical? textAlignVertical;
  final List<TextInputFormatter>? inputFormatters;
  final ValueChanged<String>? onChanged;
  final ValueChanged<String>? onSubmitted;
  final Widget? prefix;
  final Widget? suffix;

  /// Around the text; 8 by 5 inside a frame, nothing when [bare].
  final EdgeInsets? padding;
  final bool capsule;
  final bool bare;

  @override
  State<MailTextField> createState() => _MailTextFieldState();
}

class _MailTextFieldState extends State<MailTextField> {
  FocusNode? _own;
  FocusNode get _focus => widget.focusNode ?? (_own ??= FocusNode(debugLabel: 'MailTextField'));

  @override
  void initState() {
    super.initState();
    _focus.addListener(_focusChanged);
  }

  @override
  void didUpdateWidget(MailTextField old) {
    super.didUpdateWidget(old);
    if (old.focusNode != widget.focusNode) {
      (old.focusNode ?? _own)?.removeListener(_focusChanged);
      _focus.addListener(_focusChanged);
    }
  }

  @override
  void dispose() {
    _focus.removeListener(_focusChanged);
    _own?.dispose();
    super.dispose();
  }

  void _focusChanged() => setState(() {});

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final style = widget.style ?? context.roles.body;
    final field = MacosTextField(
      controller: widget.controller,
      focusNode: _focus,
      enabled: widget.enabled,
      autofocus: widget.autofocus,
      obscureText: widget.obscureText,
      minLines: widget.minLines,
      maxLines: widget.maxLines,
      expands: widget.expands,
      keyboardType: widget.keyboardType,
      textInputAction: widget.textInputAction,
      textAlign: widget.textAlign,
      textAlignVertical: widget.textAlignVertical,
      inputFormatters: widget.inputFormatters,
      onChanged: widget.onChanged,
      onSubmitted: widget.onSubmitted,
      prefix: widget.prefix,
      suffix: widget.suffix,
      style: style,
      placeholder: widget.placeholder,
      placeholderStyle: style.copyWith(color: colors.secondaryLabel),
      cursorColor: colors.accent,
      decoration: null,
      focusedDecoration: null,
      padding: widget.padding ?? (widget.bare ? EdgeInsets.zero : const EdgeInsets.symmetric(horizontal: Space.s8, vertical: 5)),
    );
    if (widget.bare) return field;
    final focused = _focus.hasFocus;
    return Opacity(
      opacity: widget.enabled ? 1 : 0.5,
      child: AnimatedContainer(
        duration: Motion.quick,
        decoration: BoxDecoration(
          color: colors.background,
          borderRadius: BorderRadius.circular(widget.capsule ? 999 : Radii.control),
          border: Border.all(
            color: focused ? colors.accent.withValues(alpha: 0.8) : colors.border,
            width: focused ? Strokes.rim : Strokes.hairline,
          ),
          boxShadow: focused ? [BoxShadow(color: colors.accent.withValues(alpha: 0.18), spreadRadius: 2)] : null,
        ),
        child: field,
      ),
    );
  }
}

/// A search field: a [MailTextField] in a capsule with a magnifier before the text and a clear
/// button once there is some.
class MailSearchField extends StatelessWidget {
  const MailSearchField({super.key, required this.controller, this.focusNode, this.onChanged, this.placeholder = 'Search'});

  final TextEditingController controller;
  final FocusNode? focusNode;
  final ValueChanged<String>? onChanged;
  final String placeholder;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return MailTextField(
      controller: controller,
      focusNode: focusNode,
      onChanged: onChanged,
      placeholder: placeholder,
      capsule: true,
      padding: const EdgeInsets.symmetric(horizontal: Space.s6, vertical: 5),
      prefix: Padding(
        padding: const EdgeInsets.only(left: Space.s10),
        child: Icon(CupertinoIcons.search, size: 14, color: colors.secondaryLabel),
      ),
      suffix: ListenableBuilder(
        listenable: controller,
        builder: (context, _) => controller.text.isEmpty
            ? const SizedBox(width: Space.s8)
            : Padding(
                padding: const EdgeInsets.only(right: Space.s4),
                child: MailIconButton(
                  icon: CupertinoIcons.xmark_circle_fill,
                  semanticLabel: 'Clear',
                  size: 20,
                  iconSize: 14,
                  onPressed: () {
                    controller.clear();
                    onChanged?.call('');
                  },
                ),
              ),
      ),
    );
  }
}

/// One choice of a [MailMenuButton]: its [label] after an optional [leading] mark.
class MailMenuItem<T> {
  const MailMenuItem(this.value, this.label, {this.leading});

  final T value;
  final String label;
  final Widget? leading;
}

/// Where [context]'s box sits in its navigator's overlay: the anchor a popover opens from.
Rect _anchorOf(BuildContext context) {
  final box = context.findRenderObject()! as RenderBox;
  final overlay = Navigator.of(context).overlay!.context.findRenderObject()! as RenderBox;
  return Rect.fromPoints(
    box.localToGlobal(Offset.zero, ancestor: overlay),
    box.localToGlobal(box.size.bottomRight(Offset.zero), ancestor: overlay),
  );
}

/// A popover's panel: the window's background in a hairline, rounded as a card, its shadow.
BoxDecoration _popoverDecoration(MailColors colors) => BoxDecoration(
  color: colors.background,
  borderRadius: BorderRadius.circular(Radii.card),
  border: Border.all(color: colors.separator, width: Strokes.hairline),
  boxShadow: colors.popoverShadow,
);

/// The face of a button that opens a popover ([MailMenuButton], [MailDateButton]): [label] after
/// an optional [leading] mark on the quiet fill, a chevron at its end.
class _PopupFace extends StatelessWidget {
  const _PopupFace({required this.label, required this.size, required this.onPressed, this.leading});

  final String label;
  final MailControlSize size;
  final VoidCallback? onPressed;
  final Widget? leading;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return _Pressable(
      onPressed: onPressed,
      semanticLabel: label,
      builder: (context, state) => Opacity(
        opacity: onPressed == null ? 0.4 : 1,
        child: AnimatedContainer(
          duration: Motion.quick,
          height: size.height,
          padding: EdgeInsets.only(left: size.padding - 2, right: Space.s6),
          decoration: BoxDecoration(
            color: Color.alphaBlend(state.pressed ? colors.pressed : (state.hovered ? colors.hover : const Color(0x00000000)), colors.fill),
            borderRadius: BorderRadius.circular(Radii.control),
            border: Border.all(color: colors.cardBorder, width: Strokes.hairline),
            boxShadow: state.focused ? _focusRing(colors) : null,
          ),
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            if (leading != null) ...[leading!, const SizedBox(width: Space.s6)],
            Flexible(
              child: Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: context.roles.body.copyWith(fontSize: size.fontSize, height: 1, fontWeight: FontWeight.w500),
              ),
            ),
            const SizedBox(width: Space.s6),
            Icon(CupertinoIcons.chevron_down, size: 10, color: colors.secondaryLabel),
          ]),
        ),
      ),
    );
  }
}

/// A button showing the chosen item of a menu, which it opens under itself: the items in a
/// popover, the chosen one checked and scrolled to. Esc or a click outside closes it.
class MailMenuButton<T> extends StatelessWidget {
  const MailMenuButton({super.key, required this.items, required this.value, required this.onChanged, this.size = MailControlSize.regular});

  final List<MailMenuItem<T>> items;
  final T? value;
  final ValueChanged<T>? onChanged;
  final MailControlSize size;

  Future<void> _open(BuildContext context) async {
    final picked = await Navigator.of(context).push(_MenuRoute<T>(
      anchor: _anchorOf(context),
      items: items,
      value: value,
      colors: context.colors,
      roles: context.roles,
    ));
    if (picked != null) onChanged?.call(picked.value);
  }

  @override
  Widget build(BuildContext context) {
    MailMenuItem<T>? chosen;
    for (final item in items) {
      if (item.value == value) chosen = item;
    }
    return Builder(
      builder: (context) => _PopupFace(
        label: chosen?.label ?? '',
        leading: chosen?.leading,
        size: size,
        onPressed: onChanged == null ? null : () => _open(context),
      ),
    );
  }
}

/// The menu of a [MailMenuButton]: a popover under its button (over it when there is no room
/// below), as wide as the button at least.
class _MenuRoute<T> extends PopupRoute<MailMenuItem<T>> {
  _MenuRoute({required this.anchor, required this.items, required this.value, required this.colors, required this.roles});

  final Rect anchor;
  final List<MailMenuItem<T>> items;
  final T? value;
  final MailColors colors;
  final TextRoles roles;

  static const _row = 26.0;
  static const _visibleRows = 12;

  /// Opens with the chosen item a few rows from the top, so a long list (times of day) shows it;
  /// never past the end, which the scroll view would spring back from.
  late final _scroll = ScrollController(
    initialScrollOffset: ((items.indexWhere((item) => item.value == value) - 4).clamp(0, math.max(0, items.length - _visibleRows)) * _row).toDouble(),
  );

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  @override
  Color? get barrierColor => null;

  @override
  bool get barrierDismissible => true;

  @override
  String? get barrierLabel => 'Close menu';

  @override
  Duration get transitionDuration => Motion.quick;

  @override
  Widget buildPage(BuildContext context, Animation<double> animation, Animation<double> secondaryAnimation) {
    final panel = Container(
      constraints: BoxConstraints(minWidth: anchor.width, maxWidth: math.max(anchor.width, 360), maxHeight: _row * _visibleRows + Space.s8),
      padding: const EdgeInsets.all(Space.s4),
      decoration: _popoverDecoration(colors),
      child: SingleChildScrollView(
        controller: _scroll,
        child: IntrinsicWidth(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              for (final item in items)
                _Pressable(
                  autofocus: item.value == value,
                  semanticLabel: item.label,
                  onPressed: () => Navigator.of(context).pop(item),
                  builder: (context, state) => Container(
                    height: _row,
                    padding: const EdgeInsets.only(left: Space.s6, right: Space.s12),
                    decoration: BoxDecoration(
                      color: state.hovered || state.focused ? colors.accent : null,
                      borderRadius: BorderRadius.circular(Radii.small + 2),
                    ),
                    child: Row(children: [
                      SizedBox(
                        width: 18,
                        child: item.value == value
                            ? Icon(CupertinoIcons.checkmark_alt, size: 13, color: state.hovered || state.focused ? const Color(0xFFFFFFFF) : colors.label)
                            : null,
                      ),
                      if (item.leading != null) ...[item.leading!, const SizedBox(width: Space.s6)],
                      Flexible(
                        child: Text(
                          item.label,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: roles.body.copyWith(height: 1, color: state.hovered || state.focused ? const Color(0xFFFFFFFF) : colors.label),
                        ),
                      ),
                    ]),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
    return FadeTransition(
      opacity: CurvedAnimation(parent: animation, curve: Motion.curve),
      child: CustomSingleChildLayout(delegate: _MenuLayout(anchor), child: panel),
    );
  }
}

class _MenuLayout extends SingleChildLayoutDelegate {
  const _MenuLayout(this.anchor);

  final Rect anchor;

  static const _gap = Space.s4;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) => BoxConstraints.loose(constraints.biggest).deflate(const EdgeInsets.all(Space.s8));

  @override
  Offset getPositionForChild(Size size, Size child) {
    final below = anchor.bottom + _gap;
    final y = below + child.height <= size.height - Space.s8 ? below : math.max(Space.s8, anchor.top - _gap - child.height);
    final x = anchor.left.clamp(Space.s8, math.max(Space.s8, size.width - child.width - Space.s8)).toDouble();
    return Offset(x, y);
  }

  @override
  bool shouldRelayout(_MenuLayout old) => old.anchor != anchor;
}

/// A button showing a date, which opens that date's month under itself to pick another day: the
/// chosen day filled with the accent, today ringed, the weeks starting on the locale's first day.
/// Arrows step the months; Esc or a click outside closes it.
class MailDateButton extends StatelessWidget {
  const MailDateButton({super.key, required this.value, required this.onChanged, this.size = MailControlSize.regular});

  final DateTime value;
  final ValueChanged<DateTime>? onChanged;
  final MailControlSize size;

  Future<void> _open(BuildContext context) async {
    final picked = await Navigator.of(context).push(_DateRoute(anchor: _anchorOf(context), value: value));
    if (picked != null) onChanged?.call(picked);
  }

  @override
  Widget build(BuildContext context) => Builder(
    builder: (context) => _PopupFace(
      label: DateFormat.yMMMEd().format(value),
      leading: Icon(CupertinoIcons.calendar, size: IconSizes.inline, color: context.colors.secondaryLabel),
      size: size,
      onPressed: onChanged == null ? null : () => _open(context),
    ),
  );
}

class _DateRoute extends PopupRoute<DateTime> {
  _DateRoute({required this.anchor, required this.value});

  final Rect anchor;
  final DateTime value;

  @override
  Color? get barrierColor => null;

  @override
  bool get barrierDismissible => true;

  @override
  String? get barrierLabel => 'Close calendar';

  @override
  Duration get transitionDuration => Motion.quick;

  @override
  Widget buildPage(BuildContext context, Animation<double> animation, Animation<double> secondaryAnimation) => FadeTransition(
    opacity: CurvedAnimation(parent: animation, curve: Motion.curve),
    child: CustomSingleChildLayout(delegate: _MenuLayout(anchor), child: _MonthPanel(value: value)),
  );
}

/// A month of days to pick from, six weeks tall so stepping months never moves the arrows.
class _MonthPanel extends StatefulWidget {
  const _MonthPanel({required this.value});

  final DateTime value;

  @override
  State<_MonthPanel> createState() => _MonthPanelState();
}

class _MonthPanelState extends State<_MonthPanel> {
  late DateTime _month = DateTime(widget.value.year, widget.value.month);

  static const _cell = 28.0;

  void _step(int months) => setState(() => _month = DateTime(_month.year, _month.month + months));

  static bool _same(DateTime a, DateTime b) => a.year == b.year && a.month == b.month && a.day == b.day;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final roles = context.roles;
    final symbols = DateFormat().dateSymbols;
    // Dart's weekdays run Monday 1 to Sunday 7; the locale's first day counts Monday as 0.
    final first = (symbols.FIRSTDAYOFWEEK + 1) % 7;
    final lead = (_month.weekday % 7 - first + 7) % 7;
    final today = DateTime.now();
    Widget day(int index) {
      final date = DateTime(_month.year, _month.month, 1 - lead + index);
      final chosen = _same(date, widget.value);
      final ink = chosen ? const Color(0xFFFFFFFF) : (date.month == _month.month ? colors.label : colors.secondaryLabel);
      return _Pressable(
        autofocus: chosen,
        semanticLabel: DateFormat.yMMMMd().format(date),
        onPressed: () => Navigator.of(context).pop(DateTime(date.year, date.month, date.day, widget.value.hour, widget.value.minute)),
        builder: (context, state) => Container(
          width: _cell,
          height: _cell,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            color: chosen ? colors.accent : (state.hovered || state.focused ? colors.hover : null),
            border: !chosen && _same(date, today) ? Border.all(color: colors.accent, width: Strokes.rim) : null,
          ),
          child: Text('${date.day}', style: roles.body.copyWith(fontSize: 12, height: 1, color: ink, fontWeight: chosen ? FontWeight.w600 : null)),
        ),
      );
    }

    return Container(
      padding: const EdgeInsets.all(Space.s8),
      decoration: _popoverDecoration(colors),
      child: SizedBox(
        width: _cell * 7,
        child: Column(mainAxisSize: MainAxisSize.min, children: [
          Row(children: [
            const SizedBox(width: Space.s4),
            Expanded(child: Text(DateFormat.yMMMM().format(_month), style: roles.headline)),
            MailIconButton(icon: CupertinoIcons.chevron_left, tooltip: 'Previous month', size: 22, iconSize: IconSizes.inline, onPressed: () => _step(-1)),
            MailIconButton(icon: CupertinoIcons.chevron_right, tooltip: 'Next month', size: 22, iconSize: IconSizes.inline, onPressed: () => _step(1)),
          ]),
          const SizedBox(height: Space.s4),
          Row(children: [
            for (var i = 0; i < 7; i++)
              SizedBox(
                width: _cell,
                child: Center(child: Text(symbols.STANDALONENARROWWEEKDAYS[(first + i) % 7], style: roles.meta)),
              ),
          ]),
          for (var week = 0; week < 6; week++) Row(children: [for (var i = 0; i < 7; i++) day(week * 7 + i)]),
        ]),
      ),
    );
  }
}

/// An on/off switch: a capsule track, the accent when on, with a white knob that slides.
class MailSwitch extends StatelessWidget {
  const MailSwitch({super.key, required this.value, required this.onChanged, this.semanticLabel});

  final bool value;
  final ValueChanged<bool>? onChanged;
  final String? semanticLabel;

  static const _width = 32.0;
  static const _height = 18.0;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Semantics(
      toggled: value,
      child: _Pressable(
        onPressed: onChanged == null ? null : () => onChanged!(!value),
        semanticLabel: semanticLabel,
        builder: (context, state) => Opacity(
          opacity: onChanged == null ? 0.4 : 1,
          child: AnimatedContainer(
            duration: Motion.quick,
            curve: Motion.curve,
            width: _width,
            height: _height,
            padding: const EdgeInsets.all(Space.s2),
            alignment: value ? Alignment.centerRight : Alignment.centerLeft,
            decoration: BoxDecoration(
              color: value ? colors.accent : colors.strongFill,
              borderRadius: BorderRadius.circular(_height / 2),
              boxShadow: state.focused ? _focusRing(colors) : null,
            ),
            child: Container(
              width: _height - 4,
              height: _height - 4,
              decoration: BoxDecoration(color: const Color(0xFFFFFFFF), shape: BoxShape.circle, boxShadow: colors.cardShadow),
            ),
          ),
        ),
      ),
    );
  }
}

/// A checkbox: a small rounded box, filled with the accent and checked when on.
class MailCheckbox extends StatelessWidget {
  const MailCheckbox({super.key, required this.value, required this.onChanged, this.semanticLabel});

  final bool value;
  final ValueChanged<bool>? onChanged;
  final String? semanticLabel;

  static const _side = 16.0;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Semantics(
      checked: value,
      child: _Pressable(
        onPressed: onChanged == null ? null : () => onChanged!(!value),
        semanticLabel: semanticLabel,
        builder: (context, state) => Opacity(
          opacity: onChanged == null ? 0.4 : 1,
          child: AnimatedContainer(
            duration: Motion.quick,
            width: _side,
            height: _side,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: value ? colors.accent : Color.alphaBlend(state.hovered ? colors.hover : const Color(0x00000000), colors.background),
              borderRadius: BorderRadius.circular(Radii.small),
              border: value ? null : Border.all(color: colors.border, width: Strokes.rim),
              boxShadow: state.focused ? _focusRing(colors) : null,
            ),
            child: value ? const Icon(CupertinoIcons.checkmark_alt, size: 13, color: Color(0xFFFFFFFF)) : null,
          ),
        ),
      ),
    );
  }
}

/// What a [MailNotice] tells: [action], something to do (sign in again), in the indigo of
/// "waiting on you"; [error], something that failed, in red.
enum MailNoticeTone { action, error }

/// A notice in a page (MS-191), the one look of every banner: [icon] (the tone's by default) in
/// the tone's color before [message], under a [title] when there is one, on a [NoticeMetrics]
/// wash of that color inside a hairline of it, [Radii.card] corners. A card ends in [trailing]
/// (a button); [compact], it is one [NoticeMetrics.compactHeight] line over a view, ending in
/// [action] in the tone's color. With [onTap] the whole notice presses, washed under the pointer.
class MailNotice extends StatelessWidget {
  const MailNotice({
    super.key,
    required this.tone,
    required this.message,
    this.title,
    this.icon,
    this.trailing,
    this.action,
    this.onTap,
    this.compact = false,
  });

  final MailNoticeTone tone;
  final String message;
  final String? title;
  final IconData? icon;
  final Widget? trailing;
  final String? action;
  final VoidCallback? onTap;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final roles = context.roles;
    final error = tone == MailNoticeTone.error;
    final hue = error ? colors.red : colors.waiting;
    final ink = error ? colors.onRedFill : colors.onWaitingFill;
    final face = hue.withValues(alpha: colors.brightness == Brightness.light ? NoticeMetrics.tint : NoticeMetrics.darkTint);
    final glyph = Icon(
      icon ?? (error ? CupertinoIcons.exclamationmark_triangle : CupertinoIcons.info_circle),
      size: compact ? IconSizes.row : IconSizes.toolbar,
      color: ink,
    );
    final content = compact
        ? Row(children: [
            glyph,
            const SizedBox(width: Space.s8),
            Expanded(
              child: Text(message, maxLines: 1, overflow: TextOverflow.ellipsis,
                  style: roles.meta.copyWith(color: colors.label, fontWeight: FontWeight.w500)),
            ),
            if (action != null) ...[
              const SizedBox(width: Space.s8),
              Text(action!, maxLines: 1, style: roles.meta.copyWith(color: ink, fontWeight: FontWeight.w500)),
            ],
          ])
        : Row(children: [
            glyph,
            const SizedBox(width: Space.s10),
            Expanded(
              child: title == null
                  ? Text(message, style: roles.body)
                  : Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
                      Text(title!, maxLines: 1, overflow: TextOverflow.ellipsis, style: roles.headline),
                      Text(message, maxLines: 2, overflow: TextOverflow.ellipsis, style: roles.meta),
                    ]),
            ),
            if (trailing != null) ...[const SizedBox(width: Space.s8), trailing!],
          ]);
    Widget frame(Color wash, {bool focused = false}) => AnimatedContainer(
      duration: Motion.quick,
      height: compact ? NoticeMetrics.compactHeight : null,
      padding: compact
          ? const EdgeInsets.symmetric(horizontal: Space.s10)
          : const EdgeInsets.symmetric(horizontal: Space.s12, vertical: Space.s10),
      decoration: BoxDecoration(
        color: Color.alphaBlend(wash, face),
        borderRadius: BorderRadius.circular(Radii.card),
        border: Border.all(color: hue.withValues(alpha: NoticeMetrics.rim), width: Strokes.hairline),
        boxShadow: focused ? _focusRing(colors) : null,
      ),
      child: content,
    );
    const none = Color(0x00000000);
    if (onTap == null) return frame(none);
    return _Pressable(
      onPressed: onTap,
      semanticLabel: message,
      builder: (context, state) => frame(state.pressed ? colors.pressed : (state.hovered ? colors.hover : none), focused: state.focused),
    );
  }
}

/// Asks before an action that cannot be taken back: a [Sheet] with [title] over [message],
/// Cancel and [confirm] (red when [destructive]). True when confirmed.
Future<bool> confirmAction(
  BuildContext context, {
  required String title,
  required String message,
  required String confirm,
  bool destructive = true,
}) async {
  final confirmed = await showSheet<bool>(
    context,
    (context) => Sheet(
      width: 400,
      title: title,
      actions: [
        MailButton(label: 'Cancel', size: MailControlSize.large, onPressed: () => Navigator.of(context).pop(false)),
        MailButton(
          label: confirm,
          size: MailControlSize.large,
          kind: destructive ? MailButtonKind.destructive : MailButtonKind.primary,
          autofocus: true,
          onPressed: () => Navigator.of(context).pop(true),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: Space.s20),
        child: Text(message, style: context.roles.body.copyWith(color: context.colors.secondaryLabel)),
      ),
    ),
  );
  return confirmed == true;
}
