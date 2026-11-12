import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart';
import '../platform.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';
import 'glass.dart';

/// Mail's sidebar, on the window's glass panel: unified mailboxes first (Inbox expands into each
/// account's inbox, each marked with its color), Chat, Calendar, then every account's folders and
/// Add Account…, which opens Settings.
class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.state, required this.onAddAccount, this.onCollapse});

  final MailState state;

  /// Opens Settings, where accounts are signed in with an OAuth client.
  final VoidCallback onAddAccount;

  /// Folds the sidebar back into the rail; nil where the sidebar is the layout's navigation.
  final VoidCallback? onCollapse;

  static const width = 232.0;
  static const unifiedRoles = [
    engine.MailboxRole.inbox, engine.MailboxRole.starred, engine.MailboxRole.drafts, engine.MailboxRole.sent, engine.MailboxRole.archive, engine.MailboxRole.junk, engine.MailboxRole.trash,
  ];

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  bool _inboxOpen = true;
  final Set<String> _openAccounts = {};

  Color _accountColor(String account) => accountColor(widget.state.accountIndex(account));

  @override
  Widget build(BuildContext context) {
    final state = widget.state;
    final rows = <Widget>[];
    for (final role in Sidebar.unifiedRoles) {
      final boxes = state.unified(role);
      if (boxes.isEmpty) continue;
      final selection = UnifiedSelection(role);
      final expandable = role == engine.MailboxRole.inbox && boxes.length > 1;
      final selected = state.mailInView && state.selection == selection;
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
            badge: box.mailbox.unread ?? 0,
            selected: state.mailInView && state.selection == box,
            onTap: () => state.select(box),
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
        onTap: state.showChat,
      ));
    }
    if (state.accounts.isNotEmpty) {
      rows.add(_Destination(
        leading: const Icon(CupertinoIcons.calendar),
        label: 'Calendar',
        selected: state.calendar.active,
        onTap: state.showCalendar,
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
      for (final box in state.mailboxes[account.id] ?? const <engine.Mailbox>[]) {
        final selected = state.mailInView && state.selection == MailboxSelection(account.id, box);
        rows.add(_Destination(
          indent: true,
          leading: Icon(roleIcon(box.role)),
          label: box.name,
          badge: _badge(box.role) ? (box.unread ?? 0) : 0,
          selected: selected,
          onTap: () => state.select(MailboxSelection(account.id, box)),
        ));
      }
    }
    rows.add(_Destination(
      leading: const Icon(CupertinoIcons.plus_circle),
      label: 'Add Account…',
      onTap: widget.onAddAccount,
    ));

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
                  ToolButton(tooltip: 'Hide sidebar', icon: CupertinoIcons.sidebar_left, onPressed: widget.onCollapse),
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

  static bool _badge(engine.MailboxRole role) => badgeRole(role);
}

bool badgeRole(engine.MailboxRole role) =>
    const {engine.MailboxRole.inbox, engine.MailboxRole.other, engine.MailboxRole.important, engine.MailboxRole.starred, engine.MailboxRole.drafts}.contains(role);

/// A sidebar section's title: small, bold, secondary, as the Mac's sidebars head their groups.
class _SectionHeader extends StatelessWidget {
  const _SectionHeader(this.title);

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(20, 14, 16, 4),
    child: Text(title, style: context.text.subheadline.copyWith(color: context.colors.secondaryLabel, fontWeight: FontWeight.w600)),
  );
}

/// The rail for medium windows: a button that opens the full sidebar with every account's
/// folders, then the unified mailboxes, each a glyph over its name.
class Rail extends StatelessWidget {
  const Rail({super.key, required this.state, required this.onMenu});

  final MailState state;
  final VoidCallback onMenu;

  static const width = 72.0;

  @override
  Widget build(BuildContext context) {
    final roles = Sidebar.unifiedRoles.where((r) => state.unified(r).isNotEmpty).toList();
    final selection = state.selection;
    final chat = state.chat;
    return SizedBox(
      width: width,
      child: Column(children: [
        WindowDragArea(
          child: Padding(
            // Room under the Mac's traffic lights; Windows' title bar is above the content.
            padding: EdgeInsets.only(top: Host.mac ? 36 : 8, bottom: 8),
            child: Center(child: ToolButton(tooltip: 'Show sidebar', icon: CupertinoIcons.sidebar_left, onPressed: onMenu)),
          ),
        ),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            children: [
              for (final role in roles)
                _RailItem(
                  icon: roleIcon(role),
                  label: role.title,
                  count: badgeRole(role) ? state.unreadCount(UnifiedSelection(role)) : 0,
                  selected: state.mailInView && selection is UnifiedSelection && selection.role == role,
                  onTap: () => state.select(UnifiedSelection(role)),
                ),
              if (chat.accounts.isNotEmpty)
                _RailItem(
                  icon: CupertinoIcons.chat_bubble_2,
                  label: 'Chat',
                  count: chat.unreadSpaces,
                  selected: chat.active,
                  onTap: state.showChat,
                ),
              if (state.accounts.isNotEmpty)
                _RailItem(
                  icon: CupertinoIcons.calendar,
                  label: 'Calendar',
                  count: 0,
                  selected: state.calendar.active,
                  onTap: state.showCalendar,
                ),
            ],
          ),
        ),
      ]),
    );
  }
}

/// A rail destination: the glyph on a rounded highlight when selected, its name below.
class _RailItem extends StatelessWidget {
  const _RailItem({required this.icon, required this.label, required this.count, required this.selected, required this.onTap});

  final IconData icon;
  final String label;
  final int count;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: GestureDetector(
        onTap: onTap,
        behavior: HitTestBehavior.opaque,
        child: Column(children: [
          Hoverable(
            onTap: onTap,
            selected: selected,
            selectedColor: colors.label.withValues(alpha: colors.brightness == Brightness.light ? 0.09 : 0.14),
            child: SizedBox(
              width: 48,
              height: 30,
              child: Center(
                child: CountBadge(count: count, child: MacosIcon(icon, size: 19, color: selected ? colors.accent : colors.secondaryLabel)),
              ),
            ),
          ),
          const SizedBox(height: 3),
          Text(label, style: context.text.subheadline.copyWith(color: selected ? colors.label : colors.secondaryLabel)),
        ]),
      ),
    );
  }
}

/// An account's [MarkDot], centered in the column the other rows' glyphs take.
class _AccountDot extends StatelessWidget {
  const _AccountDot({required this.color});

  final Color color;

  @override
  Widget build(BuildContext context) => SizedBox(width: IconSizes.toolbar, child: Center(child: MarkDot(color)));
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
        child: Hoverable(
          onTap: onTap,
          selected: selected,
          selectedColor: colors.label.withValues(alpha: light ? 0.09 : 0.14),
          child: Padding(
            padding: const EdgeInsets.only(left: 8, right: 6),
            child: Row(
              children: [
                IconTheme.merge(data: IconThemeData(color: colors.accent, size: 17), child: leading),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: context.text.body.copyWith(color: colors.label, fontWeight: selected ? FontWeight.w600 : null),
                  ),
                ),
                if (badge > 0) Text(_count(badge), style: context.text.callout.copyWith(color: colors.secondaryLabel)),
                if (expanded != null)
                  GestureDetector(
                    onTap: onToggle,
                    behavior: HitTestBehavior.opaque,
                    child: SizedBox(
                      width: 22,
                      height: 30,
                      child: AnimatedRotation(
                        turns: expanded! ? 0.25 : 0,
                        duration: Motion.short,
                        child: Icon(CupertinoIcons.chevron_right, size: 11, color: colors.secondaryLabel),
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  static String _count(int n) => n >= 1000 ? '${(n / 1000).toStringAsFixed(n >= 10000 ? 0 : 1)}k' : '$n';
}
