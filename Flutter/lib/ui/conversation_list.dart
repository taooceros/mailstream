import 'package:flutter/material.dart';

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';
import 'format.dart';

/// One line of the list: a date group header, a conversation, or a Smart Inbox bundle.
sealed class ListItem {
  const ListItem();
}

final class SectionItem extends ListItem {
  const SectionItem(this.title);
  final String title;
}

final class RowItem extends ListItem {
  const RowItem(this.conversation);
  final Conversation conversation;
}

final class BundleItem extends ListItem {
  const BundleItem(this.category, this.conversations);
  final MailCategory category;

  /// Newest first.
  final List<Conversation> conversations;
}

/// The list's lines for `conversations`: bundles fold service mail at their newest member's place,
/// headers group by date.
List<ListItem> listItems(List<Conversation> conversations, {required bool bundles, required bool sections}) {
  final now = DateTime.now();
  final grouped = <MailCategory, List<Conversation>>{};
  final lines = <ListItem>[];
  for (final c in conversations) {
    final bundle = bundles ? c.bundle : null;
    if (bundle == null) {
      lines.add(RowItem(c));
    } else if (grouped.containsKey(bundle)) {
      grouped[bundle]!.add(c);
    } else {
      final members = grouped[bundle] = [c];
      lines.add(BundleItem(bundle, members));
    }
  }
  if (!sections) return lines;
  final out = <ListItem>[];
  String? current;
  for (final line in lines) {
    final date = switch (line) {
      RowItem(:final conversation) => conversation.date,
      BundleItem(:final conversations) => conversations.first.date,
      SectionItem() => null,
    };
    final section = listSection(date, now);
    if (section != current) {
      out.add(SectionItem(section));
      current = section;
    }
    out.add(line);
  }
  return out;
}

/// The keyboard highlight, shared by the list and the window's key handling.
class ListHighlight extends ChangeNotifier {
  String? _id;
  String? get id => _id;

  set id(String? value) {
    if (value == _id) return;
    _id = value;
    notifyListeners();
  }
}

String itemId(ListItem item) => switch (item) {
  RowItem(:final conversation) => conversation.id,
  BundleItem(:final category) => 'bundle:${category.name}',
  SectionItem(:final title) => 'section:$title',
};

/// A lazily built list on fixed row heights: only the rows in view are built and laid out.
class ConversationList extends StatefulWidget {
  const ConversationList({
    super.key,
    required this.state,
    required this.items,
    required this.highlight,
    required this.onActivate,
    this.emptyText = 'No conversations',
  });

  final MailState state;
  final List<ListItem> items;
  final ListHighlight highlight;
  final void Function(ListItem item) onActivate;
  final String emptyText;

  @override
  State<ConversationList> createState() => ConversationListState();
}

class ConversationListState extends State<ConversationList> {
  final _scroll = ScrollController();

  @override
  void initState() {
    super.initState();
    widget.highlight.addListener(_reveal);
  }

  @override
  void didUpdateWidget(ConversationList oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.highlight != widget.highlight) {
      oldWidget.highlight.removeListener(_reveal);
      widget.highlight.addListener(_reveal);
    }
  }

  @override
  void dispose() {
    widget.highlight.removeListener(_reveal);
    _scroll.dispose();
    super.dispose();
  }

  double _extent(ListItem item) => item is SectionItem ? ListMetrics.sectionHeight : ListMetrics.rowHeight;

  /// Keeps the keyboard highlight in view.
  void _reveal() {
    final id = widget.highlight.id;
    if (id == null || !_scroll.hasClients) return;
    var top = 8.0;
    for (final item in widget.items) {
      if (itemId(item) == id) break;
      top += _extent(item);
    }
    final position = _scroll.position;
    if (top < position.pixels) {
      _scroll.jumpTo(top - ListMetrics.sectionHeight);
    } else if (top + ListMetrics.rowHeight > position.pixels + position.viewportDimension) {
      _scroll.jumpTo(top + ListMetrics.rowHeight * 2 - position.viewportDimension);
    }
  }

  @override
  Widget build(BuildContext context) {
    final items = widget.items;
    final palette = context.palette;
    if (items.isEmpty) {
      final loading = widget.state.isLoading;
      return Center(
        child: loading
            ? SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2, color: palette.faint))
            : Text(widget.state.listError ?? widget.emptyText, style: uiText(TextSize.body, palette.muted)),
      );
    }
    final now = DateTime.now();
    return Scrollbar(
      controller: _scroll,
      child: ListView.builder(
        controller: _scroll,
        padding: const EdgeInsets.only(top: 8, bottom: 24),
        itemCount: items.length,
        itemExtentBuilder: (index, _) => index < items.length ? _extent(items[index]) : null,
        itemBuilder: (context, index) {
          final item = items[index];
          return switch (item) {
            SectionItem(:final title) => _SectionHeader(title),
            RowItem(:final conversation) => ConversationRow(
              key: ValueKey(conversation.id),
              conversation: conversation,
              state: widget.state,
              highlight: widget.highlight,
              now: now,
              onTap: () => widget.onActivate(item),
            ),
            BundleItem() => BundleRow(
              key: ValueKey(itemId(item)),
              bundle: item,
              highlight: widget.highlight,
              now: now,
              onTap: () => widget.onActivate(item),
            ),
          };
        },
      ),
    );
  }
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader(this.title);

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(ListMetrics.inset + ListMetrics.senderX, 14, 0, 0),
    child: Text(title, style: uiText(TextSize.caption, context.palette.muted, weight: FontWeight.w600)),
  );
}

/// The rounded row behind a line: hover fill under the pointer, keyboard highlight.
class _RowFrame extends StatelessWidget {
  const _RowFrame({required this.hover, required this.highlighted, required this.child});

  final bool hover;
  final bool highlighted;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset),
      child: AnimatedContainer(
        duration: Motion.quick,
        decoration: BoxDecoration(
          color: highlighted ? palette.selection : (hover ? palette.hover : const Color(0x00000000)),
          borderRadius: BorderRadius.circular(Corner.control),
        ),
        child: child,
      ),
    );
  }
}

class ConversationRow extends StatefulWidget {
  const ConversationRow({
    super.key,
    required this.conversation,
    required this.state,
    required this.highlight,
    required this.now,
    required this.onTap,
  });

  final Conversation conversation;
  final MailState state;
  final ListHighlight highlight;
  final DateTime now;
  final VoidCallback onTap;

  @override
  State<ConversationRow> createState() => _ConversationRowState();
}

class _ConversationRowState extends State<ConversationRow> {
  bool _hover = false;

  /// Resting on a row for 80 ms loads the conversation, so opening it renders the bodies at once.
  Future<void>? _rest;

  void _enter() {
    setState(() => _hover = true);
    final rest = _rest = Future.delayed(const Duration(milliseconds: 80));
    rest.then((_) {
      if (mounted && _hover && identical(rest, _rest)) widget.state.prefetch(widget.conversation);
    });
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.conversation;
    final state = widget.state;
    final palette = context.palette;
    final brightness = context.brightness;
    final person = c.from ?? const Address(email: '?');
    final subject = splitSubject(c.subject);
    final topic = c.labels.firstOrNull;
    final senderStyle = uiText(TextSize.body, palette.text, weight: c.unread ? FontWeight.w600 : FontWeight.w400);
    final senders = c.participants.isEmpty ? person.displayName : c.participants.join(', ');
    final accountColor = accountPalette[state.accountIndex(c.account) % accountPalette.length];
    final archivable = state.selection.role == MailboxRole.inbox || c.mailboxes.contains(MailboxRole.inbox);

    return ListenableBuilder(
      listenable: widget.highlight,
      builder: (context, _) => MouseRegion(
        onEnter: (_) => _enter(),
        onExit: (_) => setState(() => _hover = false),
        cursor: SystemMouseCursors.basic,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onTap,
          child: _RowFrame(
            hover: _hover,
            highlighted: widget.highlight.id == c.id,
            child: Stack(
              children: [
                if (c.unread)
                  Positioned(
                    left: ListMetrics.dotX - 3.5,
                    top: ListMetrics.rowHeight / 2 - 3.5,
                    child: Container(width: 7, height: 7, decoration: BoxDecoration(color: palette.accent, shape: BoxShape.circle)),
                  ),
                Positioned(
                  left: ListMetrics.senderX,
                  top: (ListMetrics.rowHeight - 26) / 2,
                  child: InitialsAvatar(address: person, size: 26),
                ),
                Positioned(
                  left: ListMetrics.nameX,
                  width: ListMetrics.senderX + ListMetrics.senderWidth - ListMetrics.nameX,
                  top: 0,
                  bottom: 0,
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: Text.rich(
                      TextSpan(children: [
                        TextSpan(text: senders, style: senderStyle),
                        if (c.messageCount > 1)
                          TextSpan(text: '  ${c.messageCount}', style: uiText(TextSize.caption, palette.faint, weight: FontWeight.w500)),
                      ]),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                ),
                Positioned(
                  left: ListMetrics.subjectX,
                  right: ListMetrics.trailingInset,
                  top: 0,
                  bottom: 0,
                  child: Row(
                    children: [
                      if (c.waiting)
                        Padding(
                          padding: const EdgeInsets.only(right: 6),
                          child: Icon(Icons.reply_rounded, size: 14, color: palette.waiting),
                        ),
                      Expanded(
                        child: Text.rich(
                          TextSpan(children: [
                            if (subject.tag.isNotEmpty) TextSpan(text: '${subject.tag} ', style: uiText(TextSize.footnote, palette.faint)),
                            TextSpan(
                              text: subject.title,
                              style: uiText(TextSize.body, c.unread ? palette.text : palette.text.withValues(alpha: palette.text.a * 0.8),
                                  weight: c.unread ? FontWeight.w500 : FontWeight.w400),
                            ),
                            if ((c.snippet ?? '').isNotEmpty)
                              TextSpan(
                                text: '   ${c.snippet!.length > 160 ? c.snippet!.substring(0, 160) : c.snippet}',
                                style: uiText(TextSize.body, palette.muted.withValues(alpha: palette.muted.a * 0.75)),
                              ),
                          ]),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      // The pill is wider than the date: everything it covers fades with it.
                      AnimatedOpacity(
                        opacity: _hover ? 0 : 1,
                        duration: Motion.quick,
                        child: Row(mainAxisSize: MainAxisSize.min, children: [
                          if (c.hasAttachment) Padding(padding: const EdgeInsets.only(left: 6), child: Icon(Icons.attach_file_rounded, size: 13, color: palette.faint)),
                          if (topic != null) ...[
                            const SizedBox(width: 10),
                            Container(width: 6, height: 6, decoration: BoxDecoration(color: topicColor(topic, brightness), shape: BoxShape.circle)),
                            const SizedBox(width: 5),
                            Text(topic, style: uiText(TextSize.footnote, palette.muted)),
                          ],
                          if (c.starred) Padding(padding: const EdgeInsets.only(left: 8), child: Icon(Icons.star_rounded, size: 14, color: palette.star)),
                          SizedBox(
                            width: ListMetrics.dateWidth + 6,
                            child: Text(listDate(c.date, widget.now), textAlign: TextAlign.right, style: uiText(TextSize.footnote, palette.muted)),
                          ),
                        ]),
                      ),
                    ],
                  ),
                ),
                // Hovering cross-fades the date into the action pill.
                Positioned(
                  right: ListMetrics.trailingInset - 6,
                  top: (ListMetrics.rowHeight - 30) / 2,
                  child: IgnorePointer(
                    ignoring: !_hover,
                    child: AnimatedOpacity(
                      opacity: _hover ? 1 : 0,
                      duration: Motion.quick,
                      child: _ActionPill(children: [
                        if (c.waiting)
                          HoverIconButton(icon: Icons.task_alt_rounded, tooltip: 'Done (no longer waiting)', color: palette.waiting, size: 24, iconSize: 15, onPressed: () => state.resolveWaiting(c)),
                        if (archivable) HoverIconButton(icon: Icons.archive_outlined, tooltip: 'Archive', size: 24, iconSize: 15, onPressed: () => state.archive(c)),
                        HoverIconButton(icon: Icons.delete_outline_rounded, tooltip: 'Trash', size: 24, iconSize: 15, onPressed: () => state.trash(c)),
                        HoverIconButton(
                          icon: c.unread ? Icons.mark_email_read_outlined : Icons.mark_email_unread_outlined,
                          tooltip: c.unread ? 'Mark Read' : 'Mark Unread',
                          size: 24,
                          iconSize: 15,
                          onPressed: () => state.setRead(c, read: c.unread),
                        ),
                        HoverIconButton(
                          icon: c.starred ? Icons.star_rounded : Icons.star_border_rounded,
                          tooltip: c.starred ? 'Unstar' : 'Star',
                          color: c.starred ? palette.star : null,
                          size: 24,
                          iconSize: 15,
                          onPressed: () => state.setStarred(c, starred: !c.starred),
                        ),
                      ]),
                    ),
                  ),
                ),
                if (state.accounts.length > 1)
                  Positioned(
                    right: 3,
                    top: (ListMetrics.rowHeight - 14) / 2,
                    child: Container(width: 3, height: 14, decoration: BoxDecoration(color: accountColor, borderRadius: BorderRadius.circular(1.5))),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _ActionPill extends StatelessWidget {
  const _ActionPill({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return Container(
      height: 30,
      padding: const EdgeInsets.symmetric(horizontal: 3),
      decoration: BoxDecoration(
        color: palette.page,
        borderRadius: BorderRadius.circular(15),
        border: Border.all(color: palette.line, width: 0.5),
        boxShadow: Shadows.raised,
      ),
      child: Row(mainAxisSize: MainAxisSize.min, children: children),
    );
  }
}

/// A Smart Inbox bundle: its category, how many, who and what's newest.
class BundleRow extends StatefulWidget {
  const BundleRow({super.key, required this.bundle, required this.highlight, required this.now, required this.onTap});

  final BundleItem bundle;
  final ListHighlight highlight;
  final DateTime now;
  final VoidCallback onTap;

  @override
  State<BundleRow> createState() => _BundleRowState();
}

class _BundleRowState extends State<BundleRow> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final items = widget.bundle.conversations;
    final unread = items.where((c) => c.unread).length;
    final seen = <String>{};
    final senders = [for (final c in items) if (seen.add(c.from?.displayName ?? '')) c.from?.displayName ?? '']
        .where((s) => s.isNotEmpty)
        .take(4)
        .join(', ');
    final color = switch (widget.bundle.category) {
      MailCategory.newsletter => palette.accent,
      MailCategory.invitation => palette.success,
      _ => palette.waiting,
    };
    return ListenableBuilder(
      listenable: widget.highlight,
      builder: (context, _) => MouseRegion(
        onEnter: (_) => setState(() => _hover = true),
        onExit: (_) => setState(() => _hover = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onTap,
          child: _RowFrame(
            hover: _hover,
            highlighted: widget.highlight.id == itemId(widget.bundle),
            child: Stack(
              children: [
                if (unread > 0)
                  Positioned(
                    left: ListMetrics.dotX - 3.5,
                    top: ListMetrics.rowHeight / 2 - 3.5,
                    child: Container(width: 7, height: 7, decoration: BoxDecoration(color: palette.accent, shape: BoxShape.circle)),
                  ),
                Positioned(
                  left: ListMetrics.senderX,
                  top: (ListMetrics.rowHeight - 26) / 2,
                  child: SymbolAvatar(icon: categoryIcon(widget.bundle.category), color: color, size: 26),
                ),
                Positioned(
                  left: ListMetrics.nameX,
                  width: ListMetrics.senderX + ListMetrics.senderWidth - ListMetrics.nameX,
                  top: 0,
                  bottom: 0,
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: Row(
                      children: [
                        Text(widget.bundle.category.title, style: uiText(TextSize.body, palette.text, weight: unread > 0 ? FontWeight.w600 : FontWeight.w500)),
                        const SizedBox(width: 6),
                        Container(
                          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
                          decoration: BoxDecoration(
                            color: unread > 0 ? palette.accent.withValues(alpha: 0.12) : palette.chip,
                            borderRadius: BorderRadius.circular(8),
                          ),
                          child: Text(
                            '${unread > 0 ? unread : items.length}',
                            style: uiText(TextSize.caption, unread > 0 ? palette.accent : palette.muted, weight: FontWeight.w600),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
                Positioned(
                  left: ListMetrics.subjectX,
                  right: ListMetrics.trailingInset,
                  top: 0,
                  bottom: 0,
                  child: Row(
                    children: [
                      Expanded(
                        child: Text.rich(
                          TextSpan(children: [
                            TextSpan(text: senders, style: uiText(TextSize.body, palette.text, weight: FontWeight.w500)),
                            TextSpan(text: '   ${splitSubject(items.first.subject).title}', style: uiText(TextSize.body, palette.muted)),
                          ]),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      SizedBox(
                        width: ListMetrics.dateWidth + 6,
                        child: Text(listDate(items.first.date, widget.now), textAlign: TextAlign.right, style: uiText(TextSize.footnote, palette.muted)),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
