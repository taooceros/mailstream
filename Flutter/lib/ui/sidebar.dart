import 'package:flutter/material.dart';

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';

/// A standard Material 3 navigation drawer: unified mailboxes first (Inbox expands into each
/// account's inbox, each marked with its color), then every account's folders.
class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.state, this.onCollapse});

  final MailState state;

  /// Folds the drawer back into the rail; nil where the drawer is the layout's navigation.
  final VoidCallback? onCollapse;

  static const width = 248.0;
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
    final colors = context.colors;
    final rows = <Widget>[];
    for (final role in Sidebar.unifiedRoles) {
      final boxes = state.unified(role);
      if (boxes.isEmpty) continue;
      final selection = UnifiedSelection(role);
      final expandable = role == MailboxRole.inbox && boxes.length > 1;
      final selected = state.selection == selection;
      rows.add(_Destination(
        leading: Icon(roleIcon(role, selected: selected)),
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
            selected: state.selection == MailboxSelection(box),
            onTap: () => state.select(MailboxSelection(box)),
          ));
        }
      }
    }
    rows.add(const Padding(padding: EdgeInsets.symmetric(horizontal: 16, vertical: 8), child: Divider()));
    rows.add(Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
      child: Text('Accounts', style: context.text.titleSmall?.copyWith(color: colors.onSurfaceVariant)),
    ));
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
        final selected = state.selection == MailboxSelection(box);
        rows.add(_Destination(
          indent: true,
          leading: Icon(roleIcon(box.role, selected: selected)),
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
            child: Padding(
              padding: const EdgeInsets.fromLTRB(20, 40, 12, 12),
              child: Row(children: [
                const AppMark(),
                const SizedBox(width: 12),
                Expanded(child: Text('Mailstream', style: context.text.titleMedium?.copyWith(fontWeight: FontWeight.w600))),
                if (widget.onCollapse != null)
                  IconButton(tooltip: 'Collapse', icon: const Icon(Icons.menu_open), onPressed: widget.onCollapse),
              ]),
            ),
          ),
          Expanded(child: ListView(padding: const EdgeInsets.fromLTRB(12, 4, 12, 16), children: rows)),
          if (state.unreachable != null)
            Padding(
              padding: const EdgeInsets.all(16),
              child: Text(state.unreachable!, style: context.text.bodySmall?.copyWith(color: colors.error)),
            ),
        ],
      ),
    );
  }

  static bool _badge(MailboxRole role) => badgeRole(role);
}

bool badgeRole(MailboxRole role) =>
    const {MailboxRole.inbox, MailboxRole.other, MailboxRole.important, MailboxRole.starred, MailboxRole.drafts}.contains(role);

/// The app's mark: a mail glyph on the primary container.
class AppMark extends StatelessWidget {
  const AppMark({super.key});

  @override
  Widget build(BuildContext context) => Container(
    width: 36,
    height: 36,
    decoration: BoxDecoration(color: context.colors.primaryContainer, borderRadius: BorderRadius.circular(12)),
    child: Icon(Icons.all_inbox_rounded, size: 20, color: context.colors.onPrimaryContainer),
  );
}

/// The navigation rail for medium windows: the unified mailboxes, and a menu button that
/// opens the full drawer with every account's folders.
class Rail extends StatelessWidget {
  const Rail({super.key, required this.state, required this.onMenu});

  final MailState state;
  final VoidCallback onMenu;

  @override
  Widget build(BuildContext context) {
    final roles = Sidebar.unifiedRoles.where((r) => state.unified(r).isNotEmpty).toList();
    final selection = state.selection;
    final selected = selection is UnifiedSelection ? roles.indexOf(selection.role) : -1;
    return NavigationRail(
      backgroundColor: Colors.transparent,
      minWidth: 88,
      labelType: NavigationRailLabelType.all,
      groupAlignment: -1,
      leading: WindowDragArea(
        child: Padding(
          padding: const EdgeInsets.only(top: 36, bottom: 8),
          child: IconButton(tooltip: 'All folders', icon: const Icon(Icons.menu), onPressed: onMenu),
        ),
      ),
      selectedIndex: selected < 0 ? null : selected,
      onDestinationSelected: (i) => state.select(UnifiedSelection(roles[i])),
      destinations: [
        for (final role in roles)
          NavigationRailDestination(
            icon: _badged(role, Icon(roleIcon(role))),
            selectedIcon: _badged(role, Icon(roleIcon(role, selected: true))),
            label: Text(role.title),
          ),
      ],
    );
  }

  Widget _badged(MailboxRole role, Widget icon) {
    final count = badgeRole(role) ? state.unreadCount(UnifiedSelection(role)) : 0;
    if (count == 0) return icon;
    return Badge(label: Text(count > 999 ? '999+' : '$count'), child: icon);
  }
}

class _AccountDot extends StatelessWidget {
  const _AccountDot({required this.color});

  final Color color;

  @override
  Widget build(BuildContext context) => SizedBox(
    width: 24,
    child: Center(child: Container(width: 10, height: 10, decoration: BoxDecoration(color: color, shape: BoxShape.circle))),
  );
}

/// A navigation drawer destination: a 56 dp pill, secondary container when selected.
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
    final foreground = selected ? colors.onSecondaryContainer : colors.onSurfaceVariant;
    return Padding(
      padding: EdgeInsets.only(left: indent ? 16 : 0),
      child: SizedBox(
        height: 40,
        child: Material(
          color: selected ? colors.secondaryContainer : Colors.transparent,
          shape: const StadiumBorder(),
          clipBehavior: Clip.antiAlias,
          child: InkWell(
            onTap: onTap,
            child: Padding(
              padding: const EdgeInsets.only(left: 16, right: 12),
              child: IconTheme.merge(
                data: IconThemeData(color: foreground, size: 22),
                child: Row(
                  children: [
                    leading,
                    const SizedBox(width: 12),
                    Expanded(
                      child: Text(
                        label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: context.text.labelLarge?.copyWith(
                          color: selected ? colors.onSecondaryContainer : colors.onSurface,
                          fontWeight: selected ? FontWeight.w700 : FontWeight.w500,
                        ),
                      ),
                    ),
                    if (badge > 0) Text(_count(badge), style: context.text.labelLarge?.copyWith(color: foreground)),
                    if (expanded != null)
                      IconButton(
                        visualDensity: VisualDensity.compact,
                        iconSize: 20,
                        onPressed: onToggle,
                        icon: AnimatedRotation(
                          turns: expanded! ? 0.5 : 0,
                          duration: Durations.short4,
                          child: const Icon(Icons.expand_more),
                        ),
                      ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }

  static String _count(int n) => n >= 1000 ? '${(n / 1000).toStringAsFixed(n >= 10000 ? 0 : 1)}k' : '$n';
}
