import 'package:flutter/material.dart';

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';

/// Unified mailboxes first (Inbox expands into each account's inbox, each marked with its color
/// bar), then every account's folders under Accounts.
class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.state});

  final MailState state;

  static const width = 228.0;
  static const _unifiedRoles = [
    MailboxRole.inbox, MailboxRole.starred, MailboxRole.drafts, MailboxRole.sent, MailboxRole.archive, MailboxRole.junk, MailboxRole.trash,
  ];

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  bool _inboxOpen = true;
  final Set<String> _openAccounts = {};

  @override
  Widget build(BuildContext context) {
    final state = widget.state;
    final palette = context.palette;
    final rows = <Widget>[];
    for (final role in Sidebar._unifiedRoles) {
      final boxes = state.unified(role);
      if (boxes.isEmpty) continue;
      final selection = UnifiedSelection(role);
      final expandable = role == MailboxRole.inbox && boxes.length > 1;
      rows.add(_SidebarRow(
        icon: roleIcon(role),
        title: role.title,
        badge: _badge(role) ? state.unreadCount(selection) : 0,
        selected: state.selection == selection,
        expanded: expandable ? _inboxOpen : null,
        onToggle: () => setState(() => _inboxOpen = !_inboxOpen),
        onTap: () => state.select(selection),
      ));
      if (expandable && _inboxOpen) {
        for (final box in boxes) {
          final account = state.account(box.account);
          rows.add(_SidebarRow(
            indent: 1,
            bar: accountPalette[state.accountIndex(box.account) % accountPalette.length],
            title: account?.title ?? box.account,
            badge: box.unread ?? 0,
            selected: state.selection == MailboxSelection(box),
            onTap: () => state.select(MailboxSelection(box)),
          ));
        }
      }
    }
    rows.add(const _SectionTitle('Accounts'));
    for (final account in state.accounts) {
      final open = _openAccounts.contains(account.id);
      rows.add(_SidebarRow(
        bar: accountPalette[state.accountIndex(account.id) % accountPalette.length],
        title: account.title,
        expanded: open,
        onToggle: () => setState(() => open ? _openAccounts.remove(account.id) : _openAccounts.add(account.id)),
        onTap: () => setState(() => open ? _openAccounts.remove(account.id) : _openAccounts.add(account.id)),
      ));
      if (!open) continue;
      for (final box in state.mailboxes[account.id] ?? const <Mailbox>[]) {
        rows.add(_SidebarRow(
          indent: 1,
          icon: roleIcon(box.role),
          title: box.name,
          badge: _badge(box.role) ? (box.unread ?? 0) : 0,
          selected: state.selection == MailboxSelection(box),
          onTap: () => state.select(MailboxSelection(box)),
        ));
      }
    }

    return Container(
      width: Sidebar.width,
      color: palette.sidebar,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Room for the traffic lights; drags the window like a title bar.
          const WindowDragArea(child: SizedBox(height: 52)),
          Expanded(
            child: ListView(padding: const EdgeInsets.fromLTRB(10, 0, 10, 16), children: rows),
          ),
          if (state.unreachable != null)
            Padding(
              padding: const EdgeInsets.all(14),
              child: Text(state.unreachable!, style: uiText(TextSize.footnote, palette.danger)),
            ),
        ],
      ),
    );
  }

  static bool _badge(MailboxRole role) =>
      const {MailboxRole.inbox, MailboxRole.other, MailboxRole.important, MailboxRole.starred, MailboxRole.drafts}.contains(role);
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle(this.title);

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(10, 18, 10, 6),
    child: Text(title, style: uiText(TextSize.caption, context.palette.faint, weight: FontWeight.w600)),
  );
}

class _SidebarRow extends StatefulWidget {
  const _SidebarRow({
    required this.title,
    required this.onTap,
    this.icon,
    this.bar,
    this.badge = 0,
    this.selected = false,
    this.indent = 0,
    this.expanded,
    this.onToggle,
  });

  final String title;
  final VoidCallback onTap;
  final IconData? icon;
  final Color? bar;
  final int badge;
  final bool selected;
  final int indent;

  /// Nil for a row without children.
  final bool? expanded;
  final VoidCallback? onToggle;

  @override
  State<_SidebarRow> createState() => _SidebarRowState();
}

class _SidebarRowState extends State<_SidebarRow> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final selected = widget.selected;
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: Motion.quick,
          height: 30,
          margin: const EdgeInsets.symmetric(vertical: 1),
          padding: EdgeInsets.only(left: 8 + widget.indent * 18.0, right: 8),
          decoration: BoxDecoration(
            color: selected ? palette.selection : (_hover ? palette.hover : null),
            borderRadius: BorderRadius.circular(Corner.control - 2),
          ),
          child: Row(
            children: [
              if (widget.icon != null)
                SizedBox(width: 22, child: Icon(widget.icon, size: 16, color: selected ? palette.accent : palette.muted)),
              if (widget.bar != null)
                Container(
                  width: 3,
                  height: 14,
                  margin: const EdgeInsets.only(left: 2, right: 9),
                  decoration: BoxDecoration(color: widget.bar, borderRadius: BorderRadius.circular(1.5)),
                ),
              if (widget.icon != null) const SizedBox(width: 6),
              Expanded(
                child: Text(
                  widget.title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: uiText(TextSize.body, palette.text, weight: selected ? FontWeight.w600 : FontWeight.w400),
                ),
              ),
              if (widget.badge > 0)
                Text(_count(widget.badge), style: uiText(TextSize.footnote, palette.muted, weight: FontWeight.w500)),
              if (widget.expanded != null)
                GestureDetector(
                  onTap: widget.onToggle,
                  child: AnimatedRotation(
                    turns: widget.expanded! ? 0.25 : 0,
                    duration: Motion.quick,
                    child: Padding(
                      padding: const EdgeInsets.only(left: 4),
                      child: Icon(Icons.chevron_right_rounded, size: 16, color: _hover ? palette.muted : palette.faint),
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }

  static String _count(int n) => n >= 1000 ? '${(n / 1000).toStringAsFixed(n >= 10000 ? 0 : 1)}k' : '$n';
}
