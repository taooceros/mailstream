import 'dart:io';

import 'package:flutter/cupertino.dart'
    show CupertinoAdaptiveTextSelectionToolbar, CupertinoIcons, cupertinoDesktopTextSelectionHandleControls;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../backend/models.dart';
import '../platform.dart';
import 'controls.dart';
import 'design.dart';

/// A person's avatar, the one look for senders, Chat, guests and accounts (MS-192, the web
/// reader's `.avatar`): their picture from the app's avatar cache (else one at [photoUrl], such as
/// the picture Google Chat publishes), else their initial, or a group's [icon], in their
/// [MailColors.avatarHue] on an [AvatarMetrics.tint] wash of it. Given a [ring] (the surface under
/// avatars that overlap), it is ringed in it and its wash laid on it, so none shows through.
class PersonAvatar extends StatelessWidget {
  const PersonAvatar({super.key, required this.address, this.photo, this.photoUrl, this.icon, this.ring, this.size = ListMetrics.avatarSize});

  /// Who: the engine's avatar for them gives the letter and the hue ([avatarOf]).
  final Address address;

  /// The picture's file, when the app has one.
  final String? photo;

  /// A picture on the web, shown while there is no file.
  final String? photoUrl;

  /// A glyph in place of the initial (a Chat space without a picture).
  final IconData? icon;
  final Color? ring;
  final double size;

  @override
  Widget build(BuildContext context) {
    final avatar = avatarOf(address);
    final hue = context.colors.avatarHue(avatar.slot);
    final ring = this.ring;
    final wash = hue.withValues(alpha: AvatarMetrics.tint);
    final icon = this.icon;
    final mark = Center(
      child: icon != null
          ? MacosIcon(icon, size: size * AvatarMetrics.glyph, color: hue)
          : Text(avatar.initial, style: context.roles.headline.copyWith(color: hue, fontSize: size * AvatarMetrics.letter, height: 1)),
    );
    final photo = this.photo;
    final photoUrl = this.photoUrl;
    final cacheWidth = (size * MediaQuery.devicePixelRatioOf(context)).round();
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: ring == null ? wash : Color.alphaBlend(wash, ring),
        border: ring == null ? null : Border.all(color: ring, width: AvatarMetrics.ring, strokeAlign: BorderSide.strokeAlignOutside),
      ),
      child: photo == null && photoUrl == null
          ? mark
          : ClipOval(
              child: photo != null
                  ? Image.file(
                      File(photo),
                      width: size,
                      height: size,
                      fit: BoxFit.cover,
                      cacheWidth: cacheWidth,
                      filterQuality: FilterQuality.medium,
                      errorBuilder: (_, _, _) => mark,
                    )
                  : Image.network(
                      photoUrl!,
                      width: size,
                      height: size,
                      fit: BoxFit.cover,
                      cacheWidth: cacheWidth,
                      filterQuality: FilterQuality.medium,
                      errorBuilder: (_, _, _) => mark,
                    ),
            ),
    );
  }
}

/// An icon in the avatar slot: Smart Inbox bundles.
class IconAvatar extends StatelessWidget {
  const IconAvatar({super.key, required this.icon, required this.background, required this.foreground, this.size = ListMetrics.avatarSize});

  final IconData icon;
  final Color background;
  final Color foreground;
  final double size;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    alignment: Alignment.center,
    decoration: BoxDecoration(color: background, shape: BoxShape.circle),
    child: MacosIcon(icon, size: size * 0.5, color: foreground),
  );
}

/// A rounded pill of small print you can act on or that names a thing: recipients, attachments,
/// an event's calendar. A [leading] glyph or mark, an [onPressed] that makes the whole pill a
/// button, an [onDelete] that adds a remove button. Labels that only describe (topics, counts,
/// answers) are a [Tag].
class Pill extends StatelessWidget {
  const Pill({
    super.key,
    required this.label,
    this.leading,
    this.background,
    this.foreground,
    this.onPressed,
    this.onDelete,
    this.deleteTooltip = 'Remove',
    this.deleteIcon = CupertinoIcons.xmark,
  });

  final String label;
  final Widget? leading;
  final Color? background;
  final Color? foreground;
  final VoidCallback? onPressed;
  final VoidCallback? onDelete;
  final String deleteTooltip;
  final IconData deleteIcon;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final foreground = this.foreground ?? colors.secondaryLabel;
    final pill = Container(
      height: 22,
      padding: EdgeInsets.only(left: leading == null ? 9 : 7, right: onDelete == null ? 9 : 3),
      decoration: ShapeDecoration(
        color: background ?? colors.label.withValues(alpha: colors.brightness == Brightness.light ? 0.05 : 0.1),
        shape: const StadiumBorder(),
      ),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        if (leading != null) ...[leading!, const SizedBox(width: 5)],
        Flexible(
          child: Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.callout.copyWith(color: foreground, fontWeight: FontWeight.w500)),
        ),
        if (onDelete != null) ...[
          const SizedBox(width: 2),
          MailIconButton(icon: deleteIcon, tooltip: deleteTooltip, size: 16, iconSize: 11, color: foreground, onPressed: onDelete),
        ],
      ]),
    );
    if (onPressed == null) return pill;
    return Hoverable(onTap: onPressed, radius: 11, child: pill);
  }
}

/// A pill naming a thing with its colored [MarkDot]: an account, a calendar, a topic.
class MarkChip extends StatelessWidget {
  const MarkChip({super.key, required this.label, this.mark});

  final String label;
  final Color? mark;

  @override
  Widget build(BuildContext context) => Pill(label: label, leading: mark == null ? null : MarkDot(mark!));
}

/// An account, a calendar or a topic: a dot of its color, the one mark for them (MS-190).
class MarkDot extends StatelessWidget {
  const MarkDot(this.color, {super.key});

  final Color color;

  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: MarkMetrics.dot,
    child: DecoratedBox(decoration: BoxDecoration(color: color, shape: BoxShape.circle)),
  );
}

/// The column before a list row's avatar: an accent dot while the row is [unread]. The dot sits
/// a little right of center, clear of the lifted card's bar at the row's start.
class UnreadDot extends StatelessWidget {
  const UnreadDot({super.key, required this.unread});

  final bool unread;

  @override
  Widget build(BuildContext context) => SizedBox(
    width: MarkMetrics.unreadSlot,
    child: unread
        ? Align(
            alignment: const Alignment(1 / 3, 0),
            child: SizedBox.square(
              dimension: MarkMetrics.unread,
              child: DecoratedBox(decoration: BoxDecoration(color: context.colors.accent, shape: BoxShape.circle)),
            ),
          )
        : null,
  );
}

/// A tag (MS-190): small print that describes, not acts: a topic, a bundle's count, an
/// invitation's answer, an event's call. A [label] and/or an [icon] in [color] on a
/// [TagMetrics.tint] wash of it, fully round; neutral (the secondary label color) when [color]
/// is null. [compact] on cards and in dense rows. Squeezed (a card's foot beside its guests), it
/// is cut at its edge.
class Tag extends StatelessWidget {
  const Tag({super.key, this.label, this.icon, this.color, this.compact = false}) : assert(label != null || icon != null);

  final String? label;
  final IconData? icon;
  final Color? color;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final color = this.color ?? context.colors.secondaryLabel;
    return Container(
      height: compact ? TagMetrics.compactHeight : TagMetrics.height,
      padding: const EdgeInsets.symmetric(horizontal: TagMetrics.padding),
      decoration: ShapeDecoration(color: color.withValues(alpha: TagMetrics.tint), shape: const StadiumBorder()),
      child: UnconstrainedBox(
        constrainedAxis: Axis.vertical,
        alignment: AlignmentDirectional.centerStart,
        clipBehavior: Clip.hardEdge,
        child: Row(mainAxisSize: MainAxisSize.min, children: [
          if (icon != null) Icon(icon, size: IconSizes.inline, color: color),
          if (icon != null && label != null) const SizedBox(width: Space.s2),
          if (label != null) Text(label!, maxLines: 1, softWrap: false, style: context.roles.tag.copyWith(color: color)),
        ]),
      ),
    );
  }
}

/// Something on a tray that lifts half off it under the pointer or while pressed (MS-189): [builder]
/// draws it, `half` when it should. Only the thing under the pointer lifts: keep its element to
/// one item (a key in lists), so the state never passes to a neighbor. Without [onTap] it stays
/// down.
class Liftable extends StatefulWidget {
  const Liftable({super.key, required this.onTap, this.onHover, required this.builder});

  final VoidCallback? onTap;
  final ValueChanged<bool>? onHover;
  final Widget Function(BuildContext context, bool half) builder;

  @override
  State<Liftable> createState() => _LiftableState();
}

class _LiftableState extends State<Liftable> {
  bool _hovered = false;
  bool _pressed = false;

  void _hover(bool hovered) {
    setState(() => _hovered = hovered);
    widget.onHover?.call(hovered);
  }

  void _press(bool pressed) => setState(() => _pressed = pressed);

  @override
  Widget build(BuildContext context) {
    final enabled = widget.onTap != null;
    return MouseRegion(
      onEnter: (_) => _hover(true),
      onExit: (_) => _hover(false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: enabled ? (_) => _press(true) : null,
        onTapUp: enabled ? (_) => _press(false) : null,
        onTapCancel: enabled ? () => _press(false) : null,
        onTap: widget.onTap,
        child: widget.builder(context, enabled && (_hovered || _pressed)),
      ),
    );
  }
}

/// A row on a list island's tray (the mail, bundle, chat and agenda lists), after Dia: selected,
/// it lifts off the tray as a [LiftedCard]; under the pointer or pressed, half as far. The card is
/// inset [ListMetrics.liftGap] / 2 top and bottom, so lifted neighbors stay apart; the hit area
/// is not.
class LiftedRow extends StatelessWidget {
  const LiftedRow({super.key, required this.selected, required this.onTap, this.onHover, required this.child});

  final bool selected;
  final VoidCallback onTap;
  final ValueChanged<bool>? onHover;
  final Widget child;

  @override
  Widget build(BuildContext context) => Liftable(
    onTap: onTap,
    onHover: onHover,
    builder: (context, half) => Stack(
      fit: StackFit.passthrough,
      children: [
        if (selected || half)
          Positioned.fill(
            top: ListMetrics.liftGap / 2,
            bottom: ListMetrics.liftGap / 2,
            child: LiftedCard(selected: selected),
          ),
        child,
      ],
    ),
  );
}

/// A lifted row's card, [Radii.card] round: selected, the lifted recipe ([lifted]) with an accent
/// bar at its start; otherwise half lifted ([half]). The calendar's events take the same two
/// decorations at their own radius, without the bar.
class LiftedCard extends StatelessWidget {
  const LiftedCard({super.key, required this.selected});

  final bool selected;

  static const _radius = BorderRadius.all(Radius.circular(Radii.card));

  /// What the user opened or selected: the [MailColors.liftedCard] face, a [MailColors.liftedRim]
  /// rim and the [MailColors.liftedShadow].
  static BoxDecoration lifted(MailColors colors, BorderRadius radius) => BoxDecoration(
    gradient: LinearGradient(begin: Alignment.topCenter, end: Alignment.bottomCenter, colors: colors.liftedCard),
    borderRadius: radius,
    border: Border.all(color: colors.liftedRim, width: Strokes.rim),
    boxShadow: colors.liftedShadow,
  );

  /// Under the pointer or pressed: [face] (a card's own; [MailColors.halfLifted] for a row,
  /// which has none) in a [MailColors.cardBorder] hairline, on the [MailColors.raisedShadow].
  static BoxDecoration half(MailColors colors, BorderRadius radius, {Color? face}) => BoxDecoration(
    color: face ?? colors.halfLifted,
    borderRadius: radius,
    border: Border.all(color: colors.cardBorder, width: Strokes.hairline),
    boxShadow: colors.raisedShadow,
  );

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    if (!selected) return DecoratedBox(decoration: half(colors, _radius));
    return DecoratedBox(
      decoration: lifted(colors, _radius),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: ListMetrics.liftBarInset),
        child: Align(
          alignment: Alignment.centerLeft,
          child: Container(
            width: ListMetrics.liftBar,
            decoration: BoxDecoration(color: colors.accent, borderRadius: BorderRadius.circular(ListMetrics.liftBar / 2)),
          ),
        ),
      ),
    );
  }
}

/// A surface that answers the pointer as the Mac's sidebars and buttons do: a rounded gray wash on
/// hover, a deeper one while pressed, [selectedColor] when [selected]. Rows and cards on a tray
/// lift instead ([Liftable], [LiftedRow]).
class Hoverable extends StatefulWidget {
  const Hoverable({
    super.key,
    required this.child,
    this.onTap,
    this.onDoubleTap,
    this.onHover,
    this.selected = false,
    this.selectedColor,
    this.radius = 8,
  });

  final Widget child;
  final VoidCallback? onTap;
  final VoidCallback? onDoubleTap;
  final ValueChanged<bool>? onHover;
  final bool selected;
  final Color? selectedColor;
  final double radius;

  @override
  State<Hoverable> createState() => _HoverableState();
}

class _HoverableState extends State<Hoverable> {
  bool _hovered = false;
  bool _pressed = false;

  void _hover(bool hovered) {
    setState(() => _hovered = hovered);
    widget.onHover?.call(hovered);
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final enabled = widget.onTap != null || widget.onDoubleTap != null;
    final Color fill;
    if (widget.selected) {
      fill = widget.selectedColor ?? colors.selection;
    } else if (enabled && _pressed) {
      fill = colors.pressed;
    } else if (enabled && _hovered) {
      fill = colors.hover;
    } else {
      fill = MacosColors.transparent;
    }
    return MouseRegion(
      onEnter: (_) => _hover(true),
      onExit: (_) => _hover(false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
        onTapUp: enabled ? (_) => setState(() => _pressed = false) : null,
        onTapCancel: enabled ? () => setState(() => _pressed = false) : null,
        onTap: widget.onTap,
        onDoubleTap: widget.onDoubleTap,
        child: DecoratedBox(
          decoration: BoxDecoration(color: fill, borderRadius: BorderRadius.circular(widget.radius)),
          child: widget.child,
        ),
      ),
    );
  }
}

/// A hairline rule: the Mac's separators, across ([Axis.horizontal]) or up and down.
class Hairline extends StatelessWidget {
  const Hairline({super.key, this.axis = Axis.horizontal, this.indent = 0, this.endIndent = 0});

  final Axis axis;
  final double indent;
  final double endIndent;

  @override
  Widget build(BuildContext context) {
    final line = ColoredBox(color: context.colors.separator);
    return axis == Axis.horizontal
        ? Padding(padding: EdgeInsets.only(left: indent, right: endIndent), child: SizedBox(height: 0.5, width: double.infinity, child: line))
        : Padding(padding: EdgeInsets.only(top: indent, bottom: endIndent), child: SizedBox(width: 0.5, height: double.infinity, child: line));
  }
}

/// A count over the corner of a glyph (the rail's unread counts): a neutral capsule, as the
/// sidebar's counts are gray; red is kept for errors.
class CountBadge extends StatelessWidget {
  const CountBadge({super.key, required this.count, required this.child});

  final int count;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    if (count == 0) return child;
    final colors = context.colors;
    return Stack(
      clipBehavior: Clip.none,
      children: [
        child,
        Positioned(
          right: -Space.s10,
          top: -Space.s6,
          child: Container(
            constraints: const BoxConstraints(minWidth: TagMetrics.compactHeight),
            height: TagMetrics.compactHeight,
            padding: const EdgeInsets.symmetric(horizontal: Space.s4),
            alignment: Alignment.center,
            decoration: ShapeDecoration(
              color: colors.strongFill,
              shape: StadiumBorder(side: BorderSide(color: colors.cardBorder, width: Strokes.hairline)),
            ),
            child: Text(count > 999 ? '999+' : '$count', style: context.roles.tag.copyWith(color: colors.label, height: 1)),
          ),
        ),
      ],
    );
  }
}

/// Text in [child] selects with the pointer and copies with ⌘C, with the Mac's handles and
/// context menu.
class Selectable extends StatelessWidget {
  const Selectable({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => SelectableRegion(
    selectionControls: cupertinoDesktopTextSelectionHandleControls,
    contextMenuBuilder: (context, state) => CupertinoAdaptiveTextSelectionToolbar.buttonItems(
      anchors: state.contextMenuAnchors,
      buttonItems: state.contextMenuButtonItems,
    ),
    child: child,
  );
}

const _window = MethodChannel('mailstream/window');

/// The title bar area: drags the window, double-click zooms it. Off the Mac the system's own
/// title bar moves and maximizes the window, and this is only [child].
class WindowDragArea extends StatelessWidget {
  const WindowDragArea({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => !Host.mac
      ? child
      : GestureDetector(
          behavior: HitTestBehavior.translucent,
          onPanStart: (_) => _window.invokeMethod<void>('drag'),
          onDoubleTap: () => _window.invokeMethod<void>('zoom'),
          child: child,
        );
}

/// SF Symbols' mailbox glyphs, as Mail's sidebar shows them.
IconData roleIcon(MailboxRole role) => switch (role) {
  MailboxRole.inbox => CupertinoIcons.tray,
  MailboxRole.starred => CupertinoIcons.star,
  MailboxRole.important => CupertinoIcons.exclamationmark_circle,
  MailboxRole.sent => CupertinoIcons.paperplane,
  MailboxRole.drafts => CupertinoIcons.doc,
  MailboxRole.archive => CupertinoIcons.archivebox,
  MailboxRole.all => CupertinoIcons.envelope,
  MailboxRole.junk => CupertinoIcons.xmark_octagon,
  MailboxRole.trash => CupertinoIcons.trash,
  MailboxRole.other => CupertinoIcons.folder,
};

IconData categoryIcon(MailCategory category) => switch (category) {
  MailCategory.people => CupertinoIcons.person_2_fill,
  MailCategory.notification => CupertinoIcons.bell_fill,
  MailCategory.newsletter => CupertinoIcons.news_solid,
  MailCategory.invitation => CupertinoIcons.calendar,
};

/// A bundle's colors: a white glyph on a system color, as Mail marks its categories.
({Color background, Color foreground}) categoryColors(MailCategory category, MailColors colors) => (
  background: switch (category) {
    MailCategory.people => colors.accent,
    MailCategory.notification => const Color(0xFF5AC8FA),
    MailCategory.newsletter => const Color(0xFFFF2D55),
    MailCategory.invitation => const Color(0xFFFF9500),
  },
  foreground: MacosColors.white,
);
