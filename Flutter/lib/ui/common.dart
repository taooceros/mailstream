import 'dart:io';

import 'package:flutter/cupertino.dart'
    show CupertinoAdaptiveTextSelectionToolbar, CupertinoIcons, cupertinoDesktopTextSelectionHandleControls;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../backend/models.dart';
import 'design.dart';

/// A sender's picture from the app's avatar cache (else one at `photoUrl`, such as the picture
/// Google Chat publishes), else their initials on the gray monogram of Contacts.
class PersonAvatar extends StatelessWidget {
  const PersonAvatar({super.key, required this.address, this.photo, this.photoUrl, this.size = ListMetrics.avatarSize});

  final Address address;

  /// The picture's file, when the app has one.
  final String? photo;

  /// A picture on the web, shown while there is no file.
  final String? photoUrl;
  final double size;

  @override
  Widget build(BuildContext context) {
    final colors = avatarColors(address.email, context.brightness);
    final initials = Text(
      address.initials,
      style: TextStyle(color: colors.foreground, fontSize: size * 0.4, fontWeight: FontWeight.w600, height: 1),
    );
    final photo = this.photo;
    final photoUrl = this.photoUrl;
    if (photo == null && photoUrl == null) {
      return Container(
        width: size,
        height: size,
        alignment: Alignment.center,
        decoration: BoxDecoration(
          shape: BoxShape.circle,
          gradient: LinearGradient(
            begin: Alignment.topCenter,
            end: Alignment.bottomCenter,
            colors: [Color.lerp(colors.background, MacosColors.white, 0.18)!, colors.background],
          ),
        ),
        child: initials,
      );
    }
    final cacheWidth = (size * MediaQuery.devicePixelRatioOf(context)).round();
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: colors.background,
        border: Border.all(color: context.colors.separator.withValues(alpha: 0.6), width: 0.5),
      ),
      child: ClipOval(
        child: photo != null
            ? Image.file(
                File(photo),
                width: size,
                height: size,
                fit: BoxFit.cover,
                cacheWidth: cacheWidth,
                filterQuality: FilterQuality.medium,
                errorBuilder: (_, _, _) => Center(child: initials),
              )
            : Image.network(
                photoUrl!,
                width: size,
                height: size,
                fit: BoxFit.cover,
                cacheWidth: cacheWidth,
                filterQuality: FilterQuality.medium,
                errorBuilder: (_, _, _) => Center(child: initials),
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

/// A rounded pill of small print: tags, recipients, attachments. A [leading] glyph or mark, an
/// [onPressed] that makes the whole pill a button, an [onDelete] that adds a remove button.
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
          MacosTooltip(
            message: deleteTooltip,
            child: MacosIconButton(
              icon: MacosIcon(deleteIcon, size: 11, color: foreground),
              onPressed: onDelete,
              backgroundColor: MacosColors.transparent,
              hoverColor: colors.hover,
              shape: BoxShape.circle,
              padding: EdgeInsets.zero,
              boxConstraints: const BoxConstraints.tightFor(width: 16, height: 16),
            ),
          ),
        ],
      ]),
    );
    if (onPressed == null) return pill;
    return Hoverable(onTap: onPressed, radius: 11, child: pill);
  }
}

/// A label with a colored mark: topics take a dot, accounts a bar.
class MarkChip extends StatelessWidget {
  const MarkChip({super.key, required this.label, this.mark, this.bar = false});

  final String label;
  final Color? mark;
  final bool bar;

  @override
  Widget build(BuildContext context) => Pill(
    label: label,
    leading: mark == null
        ? null
        : Container(
            width: bar ? 4 : 7,
            height: bar ? 12 : 7,
            decoration: BoxDecoration(color: mark, borderRadius: BorderRadius.circular(bar ? 2 : 4)),
          ),
  );
}

/// A surface that answers the pointer as the Mac's lists and sidebars do: a rounded gray wash on
/// hover, a deeper one while pressed, [selectedColor] when [selected].
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

/// A count on the system red, over the corner of a glyph (the rail's unread counts).
class CountBadge extends StatelessWidget {
  const CountBadge({super.key, required this.count, required this.child});

  final int count;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    if (count == 0) return child;
    return Stack(
      clipBehavior: Clip.none,
      children: [
        child,
        Positioned(
          right: -10,
          top: -6,
          child: Container(
            constraints: const BoxConstraints(minWidth: 16),
            height: 16,
            padding: const EdgeInsets.symmetric(horizontal: 4),
            alignment: Alignment.center,
            decoration: ShapeDecoration(color: context.colors.red, shape: const StadiumBorder()),
            child: Text(
              count > 999 ? '999+' : '$count',
              style: context.text.subheadline.copyWith(color: MacosColors.white, fontWeight: FontWeight.w600, height: 1),
            ),
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

/// The title bar area: drags the window, double-click zooms it.
class WindowDragArea extends StatelessWidget {
  const WindowDragArea({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => GestureDetector(
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
