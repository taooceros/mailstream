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

/// A lazily built list on fixed row heights: only the rows in view are built and laid out. Near
/// its end, and while it does not fill the window, it asks for the next page.
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

  /// Distance from the end at which the next page loads.
  static const _pagingMargin = 900.0;

  @override
  void initState() {
    super.initState();
    widget.highlight.addListener(_reveal);
    _scroll.addListener(_pageIfNearEnd);
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

  void _pageIfNearEnd() {
    if (!_scroll.hasClients || !widget.state.canLoadMore) return;
    final position = _scroll.position;
    if (position.maxScrollExtent - position.pixels < _pagingMargin) widget.state.loadMore();
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
      _scroll.jumpTo((top - ListMetrics.sectionHeight).clamp(0, position.maxScrollExtent));
    } else if (top + ListMetrics.rowHeight > position.pixels + position.viewportDimension) {
      _scroll.jumpTo((top + ListMetrics.rowHeight * 2 - position.viewportDimension).clamp(0, position.maxScrollExtent));
    }
  }

  @override
  Widget build(BuildContext context) {
    final items = widget.items;
    final state = widget.state;
    // A page that folded into bundles may leave the window unfilled: ask for the next one.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _pageIfNearEnd();
    });
    if (items.isEmpty) {
      if (state.isLoading || state.canLoadMore) return const Center(child: CircularProgressIndicator());
      return Center(
        child: Text(state.listError ?? widget.emptyText, style: context.text.bodyLarge?.copyWith(color: context.colors.onSurfaceVariant)),
      );
    }
    final now = DateTime.now();
    final footer = state.canLoadMore ? 1 : 0;
    return Scrollbar(
      controller: _scroll,
      child: ListView.builder(
        controller: _scroll,
        padding: const EdgeInsets.only(top: 4, bottom: 24),
        itemCount: items.length + footer,
        itemExtentBuilder: (index, _) => index < items.length ? _extent(items[index]) : (index == items.length && footer == 1 ? 56 : null),
        itemBuilder: (context, index) {
          if (index == items.length) {
            return const Center(child: SizedBox(width: 24, height: 24, child: CircularProgressIndicator(strokeWidth: 3)));
          }
          final item = items[index];
          return switch (item) {
            SectionItem(:final title) => SectionHeader(title),
            RowItem(:final conversation) => ConversationRow(
              key: ValueKey(conversation.id),
              conversation: conversation,
              state: state,
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

/// A date group's header in a list.
class SectionHeader extends StatelessWidget {
  const SectionHeader(this.title, {super.key});

  final String title;

  @override
  Widget build(BuildContext context) => Container(
    alignment: Alignment.bottomLeft,
    padding: const EdgeInsets.fromLTRB(24, 0, 16, 6),
    child: Text(
      title.toUpperCase(),
      style: context.text.labelSmall?.copyWith(color: context.colors.onSurfaceVariant, fontWeight: FontWeight.w700, letterSpacing: 0.8),
    ),
  );
}

/// A Material list item's surface: rounded, hover and keyboard highlight as state layers.
class _RowSurface extends StatelessWidget {
  const _RowSurface({required this.highlighted, required this.onTap, required this.onHover, required this.child});

  final bool highlighted;
  final VoidCallback onTap;
  final ValueChanged<bool> onHover;
  final Widget child;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset, vertical: 1),
    child: Material(
      color: highlighted ? context.colors.secondaryContainer : Colors.transparent,
      borderRadius: BorderRadius.circular(16),
      clipBehavior: Clip.antiAlias,
      child: InkWell(onTap: onTap, onHover: onHover, child: child),
    ),
  );
}

/// The layout every row shares: an unread dot, the avatar, and two lines; the first ends in
/// `meta` (the date), the second in `marks`.
class _RowLayout extends StatelessWidget {
  const _RowLayout({required this.unread, required this.avatar, required this.title, required this.meta, required this.subtitle, this.marks = const []});

  final bool unread;
  final Widget avatar;
  final Widget title;
  final Widget meta;
  final Widget subtitle;
  final List<Widget> marks;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(0, 8, 14, 8),
    child: Row(
      children: [
        SizedBox(
          width: 16,
          child: unread
              ? Center(child: Container(width: 7, height: 7, decoration: BoxDecoration(color: context.colors.primary, shape: BoxShape.circle)))
              : null,
        ),
        avatar,
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(children: [Expanded(child: title), const SizedBox(width: 8), meta]),
              const SizedBox(height: 2),
              Row(children: [Expanded(child: subtitle), ...marks]),
            ],
          ),
        ),
      ],
    ),
  );
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

  /// Whether the hover actions are built: from the hover until they have faded out. Rows
  /// without them build a fraction of the widgets, which keeps scrolling cheap.
  bool _actions = false;

  /// Resting on a row for 80 ms loads the conversation, so opening it renders the bodies at once.
  Future<void>? _rest;

  void _hovered(bool hover) {
    setState(() {
      _hover = hover;
      if (hover) _actions = true;
    });
    if (!hover) return;
    final rest = _rest = Future.delayed(const Duration(milliseconds: 80));
    rest.then((_) {
      if (mounted && _hover && identical(rest, _rest)) widget.state.prefetch(widget.conversation);
    });
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.conversation;
    final state = widget.state;
    final colors = context.colors;
    final text = context.text;
    final person = c.from ?? const Address(email: '?');
    final subject = splitSubject(c.subject);
    final topic = c.labels.firstOrNull;
    final senders = c.participants.isEmpty ? person.displayName : c.participants.join(', ');
    final archivable = state.selection.role == MailboxRole.inbox || c.mailboxes.contains(MailboxRole.inbox);
    final snippet = (c.snippet ?? '').replaceAll(RegExp(r'\s+'), ' ').trim();
    final strong = c.unread ? FontWeight.w700 : FontWeight.w500;

    final meta = Row(mainAxisSize: MainAxisSize.min, children: [
      if (c.hasAttachment) Padding(padding: const EdgeInsets.only(right: 4), child: Icon(Icons.attach_file, size: 14, color: colors.outline)),
      if (state.accounts.length > 1)
        Container(
          margin: const EdgeInsets.only(right: 6),
          width: 6,
          height: 6,
          decoration: BoxDecoration(color: accountPalette[state.accountIndex(c.account) % accountPalette.length], shape: BoxShape.circle),
        ),
      Text(
        listDate(c.date, widget.now),
        style: text.labelSmall?.copyWith(fontSize: 12, color: c.unread ? colors.primary : colors.onSurfaceVariant, fontWeight: strong),
      ),
    ]);

    // On hover the actions float over the row's end on a raised pill.
    Widget actions() => Material(
      color: colors.surfaceContainerHigh,
      elevation: 2,
      shadowColor: colors.shadow,
      shape: const StadiumBorder(),
      child: IconTheme.merge(
        data: const IconThemeData(size: 20),
        child: Row(mainAxisSize: MainAxisSize.min, children: [
          if (c.waiting) IconButton(tooltip: 'Done', icon: const Icon(Icons.task_alt), onPressed: () => state.resolveWaiting(c)),
          if (archivable) IconButton(tooltip: 'Archive', icon: const Icon(Icons.archive_outlined), onPressed: () => state.archive(c)),
          IconButton(tooltip: 'Delete', icon: const Icon(Icons.delete_outline), onPressed: () => state.trash(c)),
          IconButton(
            tooltip: c.unread ? 'Mark as read' : 'Mark as unread',
            icon: Icon(c.unread ? Icons.drafts_outlined : Icons.mark_email_unread_outlined),
            onPressed: () => state.setRead(c, read: c.unread),
          ),
          IconButton(
            tooltip: c.starred ? 'Unstar' : 'Star',
            icon: Icon(c.starred ? Icons.star_rounded : Icons.star_outline_rounded, color: c.starred ? const Color(0xFFF4B400) : null),
            onPressed: () => state.setStarred(c, starred: !c.starred),
          ),
        ]),
      ),
    );

    return ListenableBuilder(
      listenable: widget.highlight,
      builder: (context, _) => _RowSurface(
        highlighted: widget.highlight.id == c.id,
        onTap: widget.onTap,
        onHover: _hovered,
        child: Stack(children: [
          _RowLayout(
            unread: c.unread,
            avatar: PersonAvatar(address: person, photo: state.avatar(person.email)),
            title: Row(children: [
              Flexible(
                child: Text(
                  senders,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: text.bodyMedium?.copyWith(color: colors.onSurface, fontWeight: strong),
                ),
              ),
              if (c.messageCount > 1)
                Padding(
                  padding: const EdgeInsets.only(left: 5),
                  child: Text('${c.messageCount}', style: text.labelSmall?.copyWith(color: colors.onSurfaceVariant)),
                ),
              if (c.waiting)
                Padding(
                  padding: const EdgeInsets.only(left: 6),
                  child: Tooltip(message: 'Waiting on you', child: Icon(Icons.reply_rounded, size: 15, color: colors.tertiary)),
                ),
            ]),
            meta: meta,
            subtitle: Text.rich(
              TextSpan(children: [
                if (subject.tag.isNotEmpty) TextSpan(text: '${subject.tag} ', style: TextStyle(color: colors.outline, fontWeight: FontWeight.w400)),
                TextSpan(
                  text: subject.title.isEmpty ? '(no subject)' : subject.title,
                  style: TextStyle(color: colors.onSurface, fontWeight: c.unread ? FontWeight.w600 : FontWeight.w400),
                ),
                if (snippet.isNotEmpty) TextSpan(text: '  —  $snippet'),
              ]),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: text.bodySmall?.copyWith(fontSize: 13, color: colors.onSurfaceVariant),
            ),
            marks: [
              if (topic != null) ...[const SizedBox(width: 8), _TopicLabel(topic)],
              if (c.starred) const Padding(padding: EdgeInsets.only(left: 6), child: Icon(Icons.star_rounded, size: 16, color: Color(0xFFF4B400))),
            ],
          ),
          if (_actions)
            Positioned(
              right: 8,
              top: 0,
              bottom: 0,
              child: Center(
                child: IgnorePointer(
                  ignoring: !_hover,
                  // Starts from transparent when first built, so the pill still fades in.
                  child: TweenAnimationBuilder<double>(
                    tween: Tween(begin: 0, end: _hover ? 1 : 0),
                    duration: Durations.short3,
                    onEnd: () {
                      if (mounted && !_hover) setState(() => _actions = false);
                    },
                    builder: (context, opacity, child) => Opacity(opacity: opacity, child: child),
                    child: actions(),
                  ),
                ),
              ),
            ),
        ]),
      ),
    );
  }
}

class _TopicLabel extends StatelessWidget {
  const _TopicLabel(this.topic);

  final String topic;

  @override
  Widget build(BuildContext context) {
    final color = topicColor(topic, context.brightness);
    return Container(
      height: 18,
      padding: const EdgeInsets.symmetric(horizontal: 7),
      alignment: Alignment.center,
      decoration: BoxDecoration(color: color.withValues(alpha: 0.12), borderRadius: BorderRadius.circular(9)),
      child: Text(topic, style: context.text.labelSmall?.copyWith(fontSize: 11, color: color, fontWeight: FontWeight.w600)),
    );
  }
}

/// A Smart Inbox bundle: its category, how many unread, who and what's newest.
class BundleRow extends StatelessWidget {
  const BundleRow({super.key, required this.bundle, required this.highlight, required this.now, required this.onTap});

  final BundleItem bundle;
  final ListHighlight highlight;
  final DateTime now;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final items = bundle.conversations;
    final unread = items.where((c) => c.unread).length;
    final seen = <String>{};
    final senders = [for (final c in items) if (seen.add(c.from?.displayName ?? '')) c.from?.displayName ?? '']
        .where((s) => s.isNotEmpty)
        .take(6)
        .join(', ');
    final tone = categoryColors(bundle.category, colors);
    final strong = unread > 0 ? FontWeight.w700 : FontWeight.w500;
    return ListenableBuilder(
      listenable: highlight,
      builder: (context, _) => _RowSurface(
        highlighted: highlight.id == itemId(bundle),
        onTap: onTap,
        onHover: (_) {},
        child: _RowLayout(
          unread: unread > 0,
          avatar: IconAvatar(icon: categoryIcon(bundle.category), background: tone.background, foreground: tone.foreground),
          title: Row(children: [
            Text(bundle.category.title, style: text.bodyMedium?.copyWith(color: colors.onSurface, fontWeight: strong)),
            const SizedBox(width: 8),
            Container(
              height: 18,
              padding: const EdgeInsets.symmetric(horizontal: 7),
              alignment: Alignment.center,
              decoration: BoxDecoration(color: tone.background, borderRadius: BorderRadius.circular(9)),
              child: Text(
                unread > 0 ? '$unread new' : '${items.length}',
                style: text.labelSmall?.copyWith(fontSize: 11, color: tone.foreground, fontWeight: FontWeight.w700),
              ),
            ),
          ]),
          meta: Text(
            listDate(items.first.date, now),
            style: text.labelSmall?.copyWith(fontSize: 12, color: unread > 0 ? colors.primary : colors.onSurfaceVariant, fontWeight: strong),
          ),
          subtitle: Text.rich(
            TextSpan(children: [
              TextSpan(text: senders, style: TextStyle(color: colors.onSurface, fontWeight: unread > 0 ? FontWeight.w600 : FontWeight.w400)),
              TextSpan(text: '  —  ${splitSubject(items.first.subject).title}'),
            ]),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: text.bodySmall?.copyWith(fontSize: 13, color: colors.onSurfaceVariant),
          ),
        ),
      ),
    );
  }
}
