import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../backend/models.dart';
import '../bench.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';
import 'reader.dart';

/// One line of the list: a date group header, a conversation, or a Smart Inbox bundle.
sealed class ListItem {
  const ListItem();
}

final class SectionItem extends ListItem {
  const SectionItem(this.title, {this.count});
  final String title;

  /// Set on the Smart Inbox's "Waiting on you" header: how many conversations it heads.
  final int? count;
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

/// The list's lines for `conversations`. In the Smart Inbox (`bundles`) service mail folds into
/// bundles pinned above the rest, newest bundle first, and what waits on the user's reply or
/// action follows under its own "Waiting on you" header, as in the native app. Headers group the
/// remaining rows by date.
List<ListItem> listItems(List<Conversation> conversations, {required bool bundles, required bool sections}) {
  final now = DateTime.now();
  final grouped = <MailCategory, List<Conversation>>{};
  final pinned = <ListItem>[];
  final waiting = <ListItem>[];
  final out = <ListItem>[];
  String? current;
  for (final c in conversations) {
    if (bundles && c.waiting) {
      waiting.add(RowItem(c));
      continue;
    }
    final bundle = bundles ? c.bundle : null;
    if (bundle != null) {
      if (grouped.containsKey(bundle)) {
        grouped[bundle]!.add(c);
      } else {
        pinned.add(BundleItem(bundle, grouped[bundle] = [c]));
      }
      continue;
    }
    if (sections) {
      final section = listSection(c.date, now);
      if (section != current) {
        out.add(SectionItem(section));
        current = section;
      }
    }
    out.add(RowItem(c));
  }
  return [
    ...pinned,
    if (waiting.isNotEmpty) SectionItem(waitingTitle, count: waiting.length),
    ...waiting,
    ...out,
  ];
}

/// A row the list marks by id: the keyboard highlight, shared by the list and the window's key
/// handling, or the row under the pointer.
class ListHighlight extends ChangeNotifier {
  String? _id;
  String? get id => _id;

  set id(String? value) {
    if (value == _id) return;
    _id = value;
    notifyListeners();
  }
}

const waitingTitle = 'Waiting on you';

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
  final _hover = ListHighlight();

  /// Distance from the end at which the next page loads.
  static const _pagingMargin = 900.0;

  @override
  void initState() {
    super.initState();
    widget.highlight.addListener(_reveal);
    _scroll.addListener(_pageIfNearEnd);
    ScrollBench.attach(_scroll);
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
    _hover.dispose();
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
    var top = 4.0;
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
      final Widget empty;
      if (state.isLoading || state.canLoadMore) {
        empty = const Center(child: Spinner());
      } else {
        empty = Center(
          child: Text(
            state.listError ?? widget.emptyText,
            style: context.text.body.copyWith(color: context.colors.secondaryLabel),
          ),
        );
      }
      return empty;
    }
    final now = DateTime.now();
    final footer = state.canLoadMore ? 1 : 0;
    // MacosApp's scroll behavior puts the Mac's overlay scrollbar on it.
    return ListView.builder(
      controller: _scroll,
      padding: const EdgeInsets.only(top: 4, bottom: 24),
      itemCount: items.length + footer,
      itemExtentBuilder: (index, _) =>
          index < items.length ? _extent(items[index]) : (index == items.length && footer == 1 ? 56 : null),
      itemBuilder: (context, index) {
        if (index == items.length) {
          return const Center(child: Spinner());
        }
        final item = items[index];
        final after = index + 1 < items.length ? items[index + 1] : null;
        final next = after == null || after is SectionItem ? null : itemId(after);
        return switch (item) {
          SectionItem(:final title, :final count) => SectionHeader(title, count: count),
          RowItem(:final conversation) => ConversationRow(
            key: ValueKey(conversation.id),
            conversation: conversation,
            state: state,
            highlight: widget.highlight,
            hover: _hover,
            next: next,
            now: now,
            onTap: () => widget.onActivate(item),
          ),
          BundleItem() => BundleRow(
            key: ValueKey(itemId(item)),
            bundle: item,
            highlight: widget.highlight,
            hover: _hover,
            next: next,
            now: now,
            onTap: () => widget.onActivate(item),
          ),
        };
      },
    );
  }
}

/// A date group's header in a list, as the Mac heads table sections.
class SectionHeader extends StatelessWidget {
  const SectionHeader(this.title, {super.key, this.count});

  final String title;

  /// Shown after the title, which then takes the "Waiting on you" accent.
  final int? count;

  @override
  Widget build(BuildContext context) {
    final style = context.text.callout.copyWith(color: context.colors.secondaryLabel, fontWeight: FontWeight.w600);
    return Container(
      alignment: Alignment.bottomLeft,
      padding: const EdgeInsets.fromLTRB(22, 0, 16, 5),
      child: Text.rich(
        TextSpan(
          children: [
            TextSpan(text: title, style: count == null ? null : TextStyle(color: context.colors.waiting)),
            if (count != null) TextSpan(text: '  $count'),
          ],
        ),
        style: style,
      ),
    );
  }
}

/// A row's surface: a [LiftedRow] on the list's tray for the highlight and the pointer, and
/// under the row a hairline from the avatar on, hidden beside a lifted row.
class _RowSurface extends StatefulWidget {
  const _RowSurface({
    required this.id,
    required this.next,
    required this.highlight,
    required this.hover,
    required this.onTap,
    required this.onHover,
    required this.child,
  });

  final String id;

  /// The row below's id, if a row follows.
  final String? next;
  final ListHighlight highlight;
  final ListHighlight hover;
  final VoidCallback onTap;
  final ValueChanged<bool> onHover;
  final Widget child;

  @override
  State<_RowSurface> createState() => _RowSurfaceState();
}

class _RowSurfaceState extends State<_RowSurface> {
  void _hover(bool hovered) {
    final hover = widget.hover;
    if (hovered) {
      hover.id = widget.id;
    } else if (hover.id == widget.id) {
      hover.id = null;
    }
    widget.onHover(hovered);
  }

  @override
  void dispose() {
    // A row archived from under the pointer gets no exit; the tree is locked until the frame ends.
    final hover = widget.hover;
    final id = widget.id;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (hover.id == id) hover.id = null;
    });
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([widget.highlight, widget.hover]),
    child: widget.child,
    builder: (context, child) {
      final highlight = widget.highlight.id;
      final hover = widget.hover.id;
      final selected = highlight == widget.id;
      final next = widget.next;
      final rule = !selected && hover != widget.id && (next == null || (next != highlight && next != hover));
      return Padding(
        padding: const EdgeInsets.symmetric(horizontal: ListMetrics.inset),
        child: DecoratedBox(
          position: DecorationPosition.foreground,
          decoration: rule ? _RowSeparator(color: context.colors.separator) : const BoxDecoration(),
          child: LiftedRow(selected: selected, onTap: widget.onTap, onHover: _hover, child: child!),
        ),
      );
    },
  );
}

/// The hairline under a row, from where its text starts.
class _RowSeparator extends Decoration {
  const _RowSeparator({required this.color});

  final Color color;

  @override
  BoxPainter createBoxPainter([VoidCallback? onChanged]) => _RowSeparatorPainter(color);

  @override
  bool operator ==(Object other) => other is _RowSeparator && other.color == color;

  @override
  int get hashCode => color.hashCode;
}

class _RowSeparatorPainter extends BoxPainter {
  _RowSeparatorPainter(this.color);

  final Color color;

  @override
  void paint(Canvas canvas, Offset offset, ImageConfiguration configuration) {
    final size = configuration.size!;
    const start = 16 + ListMetrics.avatarSize + 12;
    final y = offset.dy + size.height - 0.25;
    canvas.drawLine(Offset(offset.dx + start, y), Offset(offset.dx + size.width - 8, y), Paint()..color = color..strokeWidth = 0.5);
  }
}

/// The layout every row shares: an unread dot, the avatar, and two lines; the first ends in
/// `meta` (the date), the second in `marks`.
class _RowLayout extends StatelessWidget {
  const _RowLayout({
    required this.unread,
    required this.avatar,
    required this.title,
    required this.meta,
    required this.subtitle,
    this.marks = const [],
  });

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
        // The dot sits 6 pt in, clear of the lifted card's bar at its start.
        SizedBox(
          width: 16,
          child: unread
              ? Align(
                  alignment: const Alignment(1 / 3, 0),
                  child: Container(
                    width: 7,
                    height: 7,
                    decoration: BoxDecoration(color: context.colors.accent, shape: BoxShape.circle),
                  ),
                )
              : null,
        ),
        avatar,
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(child: title),
                  const SizedBox(width: 8),
                  meta,
                ],
              ),
              const SizedBox(height: 2),
              Row(
                children: [
                  Expanded(child: subtitle),
                  ...marks,
                ],
              ),
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
    required this.hover,
    required this.next,
    required this.now,
    required this.onTap,
  });

  final Conversation conversation;
  final MailState state;
  final ListHighlight highlight;
  final ListHighlight hover;

  /// The row below's id, if a row follows.
  final String? next;
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

  /// Resting on a row for 80 ms loads the conversation and prepares its HTML, so opening it
  /// renders the bodies at once.
  Future<void>? _rest;

  void _hovered(bool hover) {
    setState(() {
      _hover = hover;
      if (hover) _actions = true;
    });
    if (!hover) return;
    final rest = _rest = Future.delayed(const Duration(milliseconds: 80));
    rest.then((_) {
      if (mounted && _hover && identical(rest, _rest)) Reader.prefetch(widget.state, widget.conversation);
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
    final strong = c.unread ? FontWeight.w700 : FontWeight.w600;

    final meta = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (c.hasAttachment)
          Padding(
            padding: const EdgeInsets.only(right: 4),
            child: Icon(CupertinoIcons.paperclip, size: 13, color: colors.secondaryLabel),
          ),
        if (state.accounts.length > 1)
          Container(
            margin: const EdgeInsets.only(right: 6),
            width: 6,
            height: 6,
            decoration: BoxDecoration(
              color: accountPalette[state.accountIndex(c.account) % accountPalette.length],
              shape: BoxShape.circle,
            ),
          ),
        Text(listDate(c.date, widget.now), style: text.callout.copyWith(color: colors.secondaryLabel, fontWeight: FontWeight.w400)),
      ],
    );

    // On hover the actions float over the row's end on a raised capsule.
    Widget actions() => RaisedCapsule(
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (c.waiting)
            ToolButton(
              tooltip: 'Resolve (D)',
              icon: CupertinoIcons.checkmark_circle,
              onPressed: () => state.setWaiting(c, waiting: false),
            ),
          if (archivable) ToolButton(tooltip: 'Archive', icon: CupertinoIcons.archivebox, onPressed: () => state.archive(c)),
          ToolButton(tooltip: 'Delete', icon: CupertinoIcons.trash, onPressed: () => state.trash(c)),
          ToolButton(
            tooltip: c.unread ? 'Mark as read' : 'Mark as unread',
            icon: c.unread ? CupertinoIcons.envelope_open : CupertinoIcons.envelope_badge,
            onPressed: () => state.setRead(c, read: c.unread),
          ),
          ToolButton(
            tooltip: c.starred ? 'Unstar' : 'Star',
            icon: c.starred ? CupertinoIcons.star_fill : CupertinoIcons.star,
            color: c.starred ? starColor : null,
            onPressed: () => state.setStarred(c, starred: !c.starred),
          ),
        ],
      ),
    );

    return _RowSurface(
      id: c.id,
      next: widget.next,
      highlight: widget.highlight,
      hover: widget.hover,
      onTap: widget.onTap,
      onHover: _hovered,
        child: Stack(
          children: [
            _RowLayout(
              unread: c.unread,
              avatar: PersonAvatar(address: person, photo: state.avatar(person.email)),
              title: Row(
                children: [
                  Flexible(
                    child: Text(
                      senders,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.body.copyWith(color: colors.label, fontWeight: strong),
                    ),
                  ),
                  if (c.messageCount > 1)
                    Padding(
                      padding: const EdgeInsets.only(left: 5),
                      child: Text(
                        '${c.messageCount}',
                        style: text.subheadline.copyWith(color: colors.secondaryLabel),
                      ),
                    ),
                  if (c.waiting)
                    Padding(
                      padding: const EdgeInsets.only(left: 6),
                      child: MacosTooltip(
                        message: 'Waiting on you',
                        child: Icon(CupertinoIcons.arrowshape_turn_up_left_fill, size: 12, color: colors.waiting),
                      ),
                    ),
                ],
              ),
              meta: meta,
              subtitle: Text.rich(
                TextSpan(
                  children: [
                    if (subject.tag.isNotEmpty)
                      TextSpan(
                        text: '${subject.tag} ',
                        style: TextStyle(color: colors.secondaryLabel, fontWeight: FontWeight.w400),
                      ),
                    TextSpan(
                      text: subject.title.isEmpty ? '(no subject)' : subject.title,
                      style: TextStyle(
                        color: colors.label,
                        fontWeight: c.unread ? FontWeight.w500 : FontWeight.w400,
                      ),
                    ),
                    if (snippet.isNotEmpty) TextSpan(text: '  —  $snippet'),
                  ],
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: text.body.copyWith(color: colors.secondaryLabel),
              ),
              marks: [
                if (topic != null) ...[const SizedBox(width: 8), _TopicLabel(topic)],
                if (c.starred)
                  const Padding(
                    padding: EdgeInsets.only(left: 6),
                    child: Icon(CupertinoIcons.star_fill, size: 13, color: starColor),
                  ),
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
                      duration: Motion.quick,
                      onEnd: () {
                        if (mounted && !_hover) setState(() => _actions = false);
                      },
                      builder: (context, opacity, child) => Opacity(opacity: opacity, child: child),
                      child: actions(),
                    ),
                  ),
                ),
              ),
          ],
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
      child: Text(
        topic,
        style: context.text.subheadline.copyWith(fontSize: 11, color: color, fontWeight: FontWeight.w600),
      ),
    );
  }
}

/// A Smart Inbox bundle: its category, how many unread, and who sent its unread mail and what the
/// newest of it is about; with nothing unread, its latest mail.
class BundleRow extends StatelessWidget {
  const BundleRow({
    super.key,
    required this.bundle,
    required this.highlight,
    required this.hover,
    required this.next,
    required this.now,
    required this.onTap,
  });

  final BundleItem bundle;
  final ListHighlight highlight;
  final ListHighlight hover;

  /// The row below's id, if a row follows.
  final String? next;
  final DateTime now;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final items = bundle.conversations;
    final unread = items.where((c) => c.unread).length;
    // What is new, as the "N new" beside the title counts; once all is read, what came last.
    final shown = unread > 0 ? items.where((c) => c.unread) : items;
    final seen = <String>{};
    final senders = [
      for (final c in shown)
        if (seen.add(c.from?.displayName ?? '')) c.from?.displayName ?? '',
    ].where((s) => s.isNotEmpty).take(6).join(', ');
    final tone = categoryColors(bundle.category, colors);
    final strong = unread > 0 ? FontWeight.w700 : FontWeight.w600;
    return _RowSurface(
      id: itemId(bundle),
      next: next,
      highlight: highlight,
      hover: hover,
      onTap: onTap,
      onHover: (_) {},
        child: _RowLayout(
          unread: unread > 0,
          avatar: IconAvatar(
            icon: categoryIcon(bundle.category),
            background: tone.background,
            foreground: tone.foreground,
          ),
          title: Row(
            children: [
              Text(
                bundle.category.title,
                style: text.body.copyWith(color: colors.label, fontWeight: strong),
              ),
              const SizedBox(width: 8),
              Container(
                height: 18,
                padding: const EdgeInsets.symmetric(horizontal: 7),
                alignment: Alignment.center,
                decoration: BoxDecoration(color: tone.background.withValues(alpha: 0.14), borderRadius: BorderRadius.circular(9)),
                child: Text(
                  unread > 0 ? '$unread new' : '${items.length}',
                  style: text.subheadline.copyWith(color: tone.background, fontWeight: FontWeight.w600),
                ),
              ),
            ],
          ),
          meta: Text(listDate(items.first.date, now), style: text.callout.copyWith(color: colors.secondaryLabel, fontWeight: FontWeight.w400)),
          subtitle: Text.rich(
            TextSpan(
              children: [
                TextSpan(
                  text: senders,
                  style: TextStyle(color: colors.label, fontWeight: unread > 0 ? FontWeight.w600 : FontWeight.w400),
                ),
                TextSpan(text: '  —  ${splitSubject(shown.first.subject).title}'),
              ],
            ),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: text.body.copyWith(color: colors.secondaryLabel),
          ),
      ),
    );
  }
}
