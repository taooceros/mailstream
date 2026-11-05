import 'dart:io';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

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
            colors: [Color.lerp(colors.background, Colors.white, 0.18)!, colors.background],
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
({Color background, Color foreground}) categoryColors(MailCategory category, ColorScheme colors) => (
  background: switch (category) {
    MailCategory.people => colors.primary,
    MailCategory.notification => const Color(0xFF5AC8FA),
    MailCategory.newsletter => const Color(0xFFFF2D55),
    MailCategory.invitation => const Color(0xFFFF9500),
  },
  foreground: Colors.white,
);
