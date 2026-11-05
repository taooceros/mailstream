import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../backend/models.dart';
import 'design.dart';

/// Initials on the sender's stable color (`Avatar`).
class InitialsAvatar extends StatelessWidget {
  const InitialsAvatar({super.key, required this.address, this.size = ListMetrics.avatarSize});

  final Address address;
  final double size;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(color: avatarColor(address.email, context.brightness), shape: BoxShape.circle),
      child: Text(
        address.initials,
        style: TextStyle(color: Colors.white, fontSize: size * 0.43, fontWeight: FontWeight.w600, height: 1),
      ),
    );
  }
}

/// A symbol in the avatar slot: bundles and automated senders.
class SymbolAvatar extends StatelessWidget {
  const SymbolAvatar({super.key, required this.icon, required this.color, this.size = ListMetrics.avatarSize});

  final IconData icon;
  final Color color;
  final double size;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    decoration: BoxDecoration(color: color.withValues(alpha: 0.14), shape: BoxShape.circle),
    child: Icon(icon, size: size * 0.55, color: color),
  );
}

/// An icon button with the house hover fill.
class HoverIconButton extends StatefulWidget {
  const HoverIconButton({
    super.key,
    required this.icon,
    required this.tooltip,
    required this.onPressed,
    this.color,
    this.size = 28,
    this.iconSize = 17,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;
  final Color? color;
  final double size;
  final double iconSize;

  @override
  State<HoverIconButton> createState() => _HoverIconButtonState();
}

class _HoverIconButtonState extends State<HoverIconButton> {
  bool _hover = false;
  bool _down = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final enabled = widget.onPressed != null;
    return Tooltip(
      message: widget.tooltip,
      waitDuration: const Duration(milliseconds: 600),
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => _hover = true),
        onExit: (_) => setState(() => _hover = _down = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => _down = true),
          onTapCancel: () => setState(() => _down = false),
          onTapUp: (_) => setState(() => _down = false),
          onTap: widget.onPressed,
          child: AnimatedContainer(
            duration: Motion.quick,
            width: widget.size,
            height: widget.size,
            decoration: BoxDecoration(
              color: !enabled ? null : (_down ? palette.chip : (_hover ? palette.hover : null)),
              borderRadius: BorderRadius.circular(Corner.control),
            ),
            child: Icon(widget.icon, size: widget.iconSize, color: enabled ? (widget.color ?? palette.muted) : palette.faint),
          ),
        ),
      ),
    );
  }
}

/// A pill button: the reply dock's and the composer's.
class PillButton extends StatefulWidget {
  const PillButton({super.key, required this.label, required this.onPressed, this.icon, this.primary = false});

  final String label;
  final IconData? icon;
  final VoidCallback? onPressed;
  final bool primary;

  @override
  State<PillButton> createState() => _PillButtonState();
}

class _PillButtonState extends State<PillButton> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final enabled = widget.onPressed != null;
    final foreground = widget.primary ? palette.onAccent : palette.text;
    final background = widget.primary
        ? palette.accent.withValues(alpha: enabled ? (_hover ? 0.88 : 1) : 0.45)
        : (_hover ? palette.chip : palette.hover);
    return MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        child: AnimatedContainer(
          duration: Motion.quick,
          height: 30,
          padding: const EdgeInsets.symmetric(horizontal: 14),
          decoration: BoxDecoration(color: background, borderRadius: BorderRadius.circular(15)),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (widget.icon != null) ...[Icon(widget.icon, size: 15, color: foreground), const SizedBox(width: 6)],
              Text(widget.label, style: uiText(TextSize.body, foreground, weight: FontWeight.w500)),
            ],
          ),
        ),
      ),
    );
  }
}

/// A meta chip: a topic with its dot, an account with its bar, or plain text.
class MetaChip extends StatelessWidget {
  const MetaChip({super.key, required this.label, this.tint, this.mark = ChipMark.none});

  final String label;
  final Color? tint;
  final ChipMark mark;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return Container(
      height: 22,
      padding: const EdgeInsets.symmetric(horizontal: 8),
      decoration: BoxDecoration(color: palette.chip, borderRadius: BorderRadius.circular(11)),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (mark == ChipMark.dot)
            Container(width: 6, height: 6, margin: const EdgeInsets.only(right: 6), decoration: BoxDecoration(color: tint, shape: BoxShape.circle)),
          if (mark == ChipMark.bar)
            Container(width: 3, height: 11, margin: const EdgeInsets.only(right: 6), decoration: BoxDecoration(color: tint, borderRadius: BorderRadius.circular(1.5))),
          Text(label, style: uiText(TextSize.footnote, mark == ChipMark.none && tint != null ? tint! : palette.muted, weight: FontWeight.w500)),
        ],
      ),
    );
  }
}

enum ChipMark { none, dot, bar }

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

IconData roleIcon(MailboxRole role) => switch (role) {
  MailboxRole.inbox => Icons.inbox_outlined,
  MailboxRole.starred => Icons.star_border_rounded,
  MailboxRole.important => Icons.label_important_outline_rounded,
  MailboxRole.sent => Icons.send_outlined,
  MailboxRole.drafts => Icons.edit_note_rounded,
  MailboxRole.archive => Icons.archive_outlined,
  MailboxRole.all => Icons.all_inbox_outlined,
  MailboxRole.junk => Icons.report_gmailerrorred_outlined,
  MailboxRole.trash => Icons.delete_outline_rounded,
  MailboxRole.other => Icons.folder_outlined,
};

IconData categoryIcon(MailCategory category) => switch (category) {
  MailCategory.people => Icons.people_outline_rounded,
  MailCategory.notification => Icons.notifications_none_rounded,
  MailCategory.newsletter => Icons.newspaper_rounded,
  MailCategory.invitation => Icons.calendar_today_rounded,
};
