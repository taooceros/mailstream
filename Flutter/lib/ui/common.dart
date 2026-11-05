import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../backend/models.dart';
import 'design.dart';

/// A sender's picture from the app's avatar cache (else one at `photoUrl`, such as the picture
/// Google Chat publishes), else their initial on a stable tonal color.
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
      style: TextStyle(color: colors.foreground, fontSize: size * 0.42, fontWeight: FontWeight.w500, height: 1),
    );
    final photo = this.photo;
    final photoUrl = this.photoUrl;
    if (photo == null && photoUrl == null) return CircleAvatar(radius: size / 2, backgroundColor: colors.background, child: initials);
    final cacheWidth = (size * MediaQuery.devicePixelRatioOf(context)).round();
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: colors.background,
        border: Border.all(color: context.colors.outlineVariant.withValues(alpha: 0.6), width: 0.5),
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
  Widget build(BuildContext context) => CircleAvatar(
    radius: size / 2,
    backgroundColor: background,
    child: Icon(icon, size: size * 0.5, color: foreground),
  );
}

/// A label with a colored mark: topics take a dot, accounts a bar.
class MarkChip extends StatelessWidget {
  const MarkChip({super.key, required this.label, this.mark, this.bar = false});

  final String label;
  final Color? mark;
  final bool bar;

  @override
  Widget build(BuildContext context) => Chip(
    visualDensity: VisualDensity.compact,
    materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
    labelStyle: context.text.labelMedium?.copyWith(color: context.colors.onSurfaceVariant),
    avatar: mark == null
        ? null
        : Container(
            width: bar ? 4 : 8,
            height: bar ? 14 : 8,
            decoration: BoxDecoration(color: mark, borderRadius: BorderRadius.circular(bar ? 2 : 4)),
          ),
    label: Text(label),
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

IconData roleIcon(MailboxRole role, {bool selected = false}) => switch (role) {
  MailboxRole.inbox => selected ? Icons.inbox : Icons.inbox_outlined,
  MailboxRole.starred => selected ? Icons.star : Icons.star_outline,
  MailboxRole.important => selected ? Icons.label_important : Icons.label_important_outline,
  MailboxRole.sent => selected ? Icons.send : Icons.send_outlined,
  MailboxRole.drafts => selected ? Icons.insert_drive_file : Icons.insert_drive_file_outlined,
  MailboxRole.archive => selected ? Icons.archive : Icons.archive_outlined,
  MailboxRole.all => selected ? Icons.mail : Icons.mail_outline,
  MailboxRole.junk => selected ? Icons.report : Icons.report_outlined,
  MailboxRole.trash => selected ? Icons.delete : Icons.delete_outline,
  MailboxRole.other => selected ? Icons.label : Icons.label_outline,
};

IconData categoryIcon(MailCategory category) => switch (category) {
  MailCategory.people => Icons.people_outline,
  MailCategory.notification => Icons.notifications_outlined,
  MailCategory.newsletter => Icons.newspaper_outlined,
  MailCategory.invitation => Icons.event_outlined,
};

/// A bundle's tonal pair from the scheme.
({Color background, Color foreground}) categoryColors(MailCategory category, ColorScheme colors) => switch (category) {
  MailCategory.newsletter => (background: colors.primaryContainer, foreground: colors.onPrimaryContainer),
  MailCategory.invitation => (background: colors.tertiaryContainer, foreground: colors.onTertiaryContainer),
  _ => (background: colors.secondaryContainer, foreground: colors.onSecondaryContainer),
};
