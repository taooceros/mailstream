import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/material.dart';

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';

/// Mail's sidebar, on the window's glass panel: unified mailboxes first (Inbox expands into each
/// account's inbox, each marked with its color), Chat, then every account's folders.
class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.state, this.onCollapse});

  final MailState state;

  /// Folds the sidebar back into the rail; nil where the sidebar is the layout's navigation.
  final VoidCallback? onCollapse;

  static const width = 232.0;
  static const unifiedRoles = [
    MailboxRole.inbox, MailboxRole.starred, MailboxRole.drafts, MailboxRole.sent, MailboxRole.archive, MailboxRole.junk, MailboxRole.trash,
  ];

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  bool _inboxOpen = true;
  final Set<String> _openAccounts = {};

  Color _accountColor(String account) => accountPalette[widget.state.accountIndex(account) % accountPalette.length];

  @override
  Widget build(BuildContext context) {
    final state = widget.state;
    final rows = <Widget>[];
    for (final role in Sidebar.unifiedRoles) {
      final boxes = state.unified(role);
      if (boxes.isEmpty) continue;
      final selection = UnifiedSelection(role);
      final expandable = role == MailboxRole.inbox && boxes.length > 1;
      final selected = !state.chat.active && state.selection == selection;
      rows.add(_Destination(
        leading: Icon(roleIcon(role)),
        label: role.title,
        badge: _badge(role) ? state.unreadCount(selection) : 0,
        selected: selected,
        expanded: expandable ? _inboxOpen : null,
        onToggle: () => setState(() => _inboxOpen = !_inboxOpen),
        onTap: () => state.select(selection),
      ));
      if (expandable && _inboxOpen) {
        for (final box in boxes) {
          rows.add(_Destination(
            indent: true,
            leading: _AccountDot(color: _accountColor(box.account)),
            label: state.account(box.account)?.title ?? box.account,
            badge: box.unread ?? 0,
            selected: !state.chat.active && state.selection == MailboxSelection(box),
            onTap: () => state.select(MailboxSelection(box)),
          ));
        }
      }
    }
    final chat = state.chat;
    if (chat.accounts.isNotEmpty) {
      rows.add(_Destination(
        leading: const Icon(CupertinoIcons.chat_bubble_2),
        label: 'Chat',
        badge: chat.unreadSpaces,
        selected: chat.active,
        onTap: chat.show,
      ));
    }
    rows.add(const _SectionHeader('Accounts'));
    for (final account in state.accounts) {
      final open = _openAccounts.contains(account.id);
      void toggle() => setState(() => open ? _openAccounts.remove(account.id) : _openAccounts.add(account.id));
      rows.add(_Destination(
        leading: _AccountDot(color: _accountColor(account.id)),
        label: account.title,
        expanded: open,
        onToggle: toggle,
        onTap: toggle,
      ));
      if (!open) continue;
      for (final box in state.mailboxes[account.id] ?? const <Mailbox>[]) {
        final selected = !state.chat.active && state.selection == MailboxSelection(box);
        rows.add(_Destination(
          indent: true,
          leading: Icon(roleIcon(box.role)),
          label: box.name,
          badge: _badge(box.role) ? (box.unread ?? 0) : 0,
          selected: selected,
          onTap: () => state.select(MailboxSelection(box)),
        ));
      }
    }

    return SizedBox(
      width: Sidebar.width,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Room for the traffic lights; drags the window like a title bar.
          WindowDragArea(
            child: SizedBox(
              height: 44,
              child: Row(children: [
                const Spacer(),
                if (widget.onCollapse != null)
                  IconButton(tooltip: 'Hide sidebar', icon: const Icon(CupertinoIcons.sidebar_left), onPressed: widget.onCollapse),
                const SizedBox(width: 8),
              ]),
            ),
          ),
          const _SectionHeader('Mailboxes'),
          Expanded(child: ListView(padding: const EdgeInsets.fromLTRB(10, 0, 10, 12), children: rows)),
        ],
      ),
    );
  }

  static bool _badge(MailboxRole role) => badgeRole(role);
}

bool badgeRole(MailboxRole role) =>
    const {MailboxRole.inbox, MailboxRole.other, MailboxRole.important, MailboxRole.starred, MailboxRole.drafts}.contains(role);

/// A sidebar section's title: small, bold, secondary, as the Mac's sidebars head their groups.
class _SectionHeader extends StatelessWidget {
  const _SectionHeader(this.title);

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(20, 14, 16, 4),
    child: Text(title, style: context.text.labelSmall?.copyWith(color: context.colors.onSurfaceVariant, fontWeight: FontWeight.w600)),
  );
}

/// The rail for medium windows: a button that opens the full sidebar with every account's
/// folders, then the unified mailboxes.
class Rail extends StatelessWidget {
  const Rail({super.key, required this.state, required this.onMenu});

  final MailState state;
  final VoidCallback onMenu;

  @override
  Widget build(BuildContext context) {
    final roles = Sidebar.unifiedRoles.where((r) => state.unified(r).isNotEmpty).toList();
    final selection = state.selection;
    final chat = state.chat;
    final hasChat = chat.accounts.isNotEmpty;
    final selected = chat.active ? roles.length : (selection is UnifiedSelection ? roles.indexOf(selection.role) : -1);
    return NavigationRail(
      minWidth: 72,
      labelType: NavigationRailLabelType.all,
      groupAlignment: -1,
      leading: WindowDragArea(
        child: Padding(
          padding: const EdgeInsets.only(top: 36, bottom: 8),
          child: IconButton(tooltip: 'Show sidebar', icon: const Icon(CupertinoIcons.sidebar_left), onPressed: onMenu),
        ),
      ),
      selectedIndex: selected < 0 ? null : selected,
      onDestinationSelected: (i) => i == roles.length ? chat.show() : state.select(UnifiedSelection(roles[i])),
      destinations: [
        for (final role in roles)
          NavigationRailDestination(icon: _badged(role, Icon(roleIcon(role))), label: Text(role.title)),
        if (hasChat)
          NavigationRailDestination(
            icon: _count(chat.unreadSpaces, const Icon(CupertinoIcons.chat_bubble_2)),
            label: const Text('Chat'),
          ),
      ],
    );
  }

  Widget _badged(MailboxRole role, Widget icon) =>
      _count(badgeRole(role) ? state.unreadCount(UnifiedSelection(role)) : 0, icon);

  static Widget _count(int count, Widget icon) {
    if (count == 0) return icon;
    return Badge(label: Text(count > 999 ? '999+' : '$count'), child: icon);
  }
}

class _AccountDot extends StatelessWidget {
  const _AccountDot({required this.color});

  final Color color;

  @override
  Widget build(BuildContext context) => SizedBox(
    width: 20,
    child: Center(child: Container(width: 9, height: 9, decoration: BoxDecoration(color: color, shape: BoxShape.circle))),
  );
}

/// A sidebar row: an accent glyph and the name, the unread count at the trailing end, a
/// rounded gray highlight when selected.
class _Destination extends StatelessWidget {
  const _Destination({
    required this.leading,
    required this.label,
    required this.onTap,
    this.badge = 0,
    this.selected = false,
    this.indent = false,
    this.expanded,
    this.onToggle,
  });

  final Widget leading;
  final String label;
  final VoidCallback onTap;
  final int badge;
  final bool selected;
  final bool indent;

  /// Nil for a destination without children.
  final bool? expanded;
  final VoidCallback? onToggle;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final light = colors.brightness == Brightness.light;
    return Padding(
      padding: EdgeInsets.only(left: indent ? 18 : 0, bottom: 1),
      child: SizedBox(
        height: 30,
        child: Material(
          color: selected ? colors.onSurface.withValues(alpha: light ? 0.09 : 0.14) : Colors.transparent,
          borderRadius: BorderRadius.circular(8),
          clipBehavior: Clip.antiAlias,
          child: InkWell(
            onTap: onTap,
            child: Padding(
              padding: const EdgeInsets.only(left: 8, right: 6),
              child: Row(
                children: [
                  IconTheme.merge(data: IconThemeData(color: colors.primary, size: 17), child: leading),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: context.text.bodyMedium?.copyWith(color: colors.onSurface, fontWeight: selected ? FontWeight.w600 : null),
                    ),
                  ),
                  if (badge > 0) Text(_count(badge), style: context.text.labelMedium?.copyWith(color: colors.onSurfaceVariant)),
                  if (expanded != null)
                    GestureDetector(
                      onTap: onToggle,
                      behavior: HitTestBehavior.opaque,
                      child: SizedBox(
                        width: 22,
                        height: 30,
                        child: AnimatedRotation(
                          turns: expanded! ? 0.25 : 0,
                          duration: Durations.short4,
                          child: Icon(CupertinoIcons.chevron_right, size: 11, color: colors.onSurfaceVariant),
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  static String _count(int n) => n >= 1000 ? '${(n / 1000).toStringAsFixed(n >= 10000 ? 0 : 1)}k' : '$n';
}
