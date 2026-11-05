import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'conversation_list.dart';
import 'design.dart';
import 'reader.dart';
import 'sidebar.dart';
import 'slide_layer.dart';

/// The window: sidebar, then a content area whose toolbar and pages follow the state. The list
/// stays mounted under the bundle and the reader, which slide over it.
class MailShell extends StatefulWidget {
  const MailShell({super.key, required this.state});

  final MailState state;

  @override
  State<MailShell> createState() => _MailShellState();
}

class _MailShellState extends State<MailShell> {
  final _focus = FocusNode(debugLabel: 'Mail window');
  final _searchFocus = FocusNode(debugLabel: 'Search');
  final _search = TextEditingController();
  final _highlight = ListHighlight();
  final _bundleHighlight = ListHighlight();
  /// A fresh reader, with its own composer and folds, per conversation.
  var _reader = GlobalKey<ReaderState>();
  String? _readerId;

  MailState get state => widget.state;

  /// What the list and the open bundle show, for drawing and for the keyboard.
  List<ListItem> _items = const [];
  List<ListItem> _bundleItems = const [];

  @override
  void initState() {
    super.initState();
    FocusManager.instance.addListener(_keepFocus);
  }

  /// Keys reach the window again once the field that had focus goes away (a closed composer).
  void _keepFocus() {
    final primary = FocusManager.instance.primaryFocus;
    if (primary == null || primary is FocusScopeNode) Future.microtask(() => mounted ? _focus.requestFocus() : null);
  }

  @override
  void dispose() {
    FocusManager.instance.removeListener(_keepFocus);
    _focus.dispose();
    _searchFocus.dispose();
    _search.dispose();
    _highlight.dispose();
    _bundleHighlight.dispose();
    super.dispose();
  }

  void _activate(ListItem item) {
    _focus.requestFocus();
    switch (item) {
      case RowItem(:final conversation):
        (state.openBundle != null ? _bundleHighlight : _highlight).id = conversation.id;
        state.open(conversation);
      case BundleItem(:final category):
        _highlight.id = itemId(item);
        _bundleHighlight.id = null;
        state.showBundle(category);
      case SectionItem():
        break;
    }
  }

  void _back() {
    if (state.opened == null && state.openBundle == null && state.searchQuery != null) {
      _search.clear();
      state.search(null);
    } else {
      state.close();
    }
    _focus.requestFocus();
  }

  bool get _editing => FocusManager.instance.primaryFocus?.context?.findAncestorWidgetOfExactType<EditableText>() != null;

  /// The highlighted conversation, else the open one.
  Conversation? get _target {
    if (state.opened != null) return state.opened;
    final highlight = state.openBundle != null ? _bundleHighlight : _highlight;
    final items = state.openBundle != null ? _bundleItems : _items;
    for (final item in items) {
      if (item is RowItem && item.conversation.id == highlight.id) return item.conversation;
    }
    return null;
  }

  void _move(int step) {
    final highlight = state.openBundle != null ? _bundleHighlight : _highlight;
    final items = (state.openBundle != null ? _bundleItems : _items).where((i) => i is! SectionItem).toList();
    if (items.isEmpty) return;
    final index = items.indexWhere((i) => itemId(i) == highlight.id);
    final next = index < 0 ? (step > 0 ? 0 : items.length - 1) : (index + step).clamp(0, items.length - 1);
    highlight.id = itemId(items[next]);
    final item = items[next];
    if (item is RowItem) state.prefetch(item.conversation);
  }

  void _openHighlighted() {
    final highlight = state.openBundle != null ? _bundleHighlight : _highlight;
    final items = state.openBundle != null ? _bundleItems : _items;
    for (final item in items) {
      if (itemId(item) == highlight.id) return _activate(item);
    }
  }

  /// After the highlighted row leaves the list, the highlight moves to its neighbor.
  void _act(void Function(Conversation) action) {
    final target = _target;
    if (target == null) return;
    if (state.opened == null) _move(1);
    action(target);
  }

  KeyEventResult _onKey(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) return KeyEventResult.ignored;
    final key = event.logicalKey;
    final keyboard = HardwareKeyboard.instance;
    if (keyboard.isMetaPressed) {
      if (key == LogicalKeyboardKey.keyF) {
        _searchFocus.requestFocus();
        _search.selection = TextSelection(baseOffset: 0, extentOffset: _search.text.length);
      } else if (key == LogicalKeyboardKey.keyR) {
        state.refresh();
      } else if (key == LogicalKeyboardKey.bracketLeft) {
        _back();
      } else {
        return KeyEventResult.ignored;
      }
      return KeyEventResult.handled;
    }
    if (_editing) return KeyEventResult.ignored;
    final reading = state.opened != null;
    switch (key) {
      case LogicalKeyboardKey.escape:
        _back();
      case LogicalKeyboardKey.arrowDown || LogicalKeyboardKey.keyJ when !reading:
        _move(1);
      case LogicalKeyboardKey.arrowUp || LogicalKeyboardKey.keyK when !reading:
        _move(-1);
      case LogicalKeyboardKey.enter || LogicalKeyboardKey.space when !reading:
        _openHighlighted();
      case LogicalKeyboardKey.keyE:
        _act(state.archive);
      case LogicalKeyboardKey.backspace || LogicalKeyboardKey.delete || LogicalKeyboardKey.numberSign:
        _act(state.trash);
      case LogicalKeyboardKey.keyS:
        _act((c) => state.setStarred(c, starred: !c.starred));
      case LogicalKeyboardKey.keyU:
        _act((c) => state.setRead(c, read: c.unread));
      case LogicalKeyboardKey.keyR when reading:
        _reader.currentState?.reply(all: keyboard.isShiftPressed);
      case LogicalKeyboardKey.keyA when reading:
        _reader.currentState?.reply(all: true);
      default:
        return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
  }

  String get _title {
    if (state.searchQuery != null) return 'Search';
    return switch (state.selection) {
      UnifiedSelection(:final role) => role.title,
      MailboxSelection(:final mailbox) => '${mailbox.name} · ${state.account(mailbox.account)?.title ?? mailbox.account}',
    };
  }

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return ListenableBuilder(
      listenable: state,
      builder: (context, _) {
        _items = listItems(state.conversations, bundles: state.bundlesEnabled, sections: state.searchQuery == null);
        final bundle = state.openBundle;
        _bundleItems = bundle == null ? const [] : listItems(state.bundleItems(bundle), bundles: false, sections: true);
        final opened = state.opened;
        if (opened != null && opened.id != _readerId) {
          _readerId = opened.id;
          _reader = GlobalKey<ReaderState>();
        }
        return Focus(
          focusNode: _focus,
          autofocus: true,
          onKeyEvent: _onKey,
          child: Listener(
            onPointerDown: (event) {
              if (isBackButton(event)) _back();
            },
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Sidebar(state: state),
                VerticalDivider(width: 0.5, thickness: 0.5, color: palette.line),
                Expanded(
                  child: ColoredBox(
                    color: palette.page,
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        _toolbar(context),
                        Divider(height: 0.5, thickness: 0.5, color: palette.line),
                        Expanded(
                          child: ClipRect(
                            child: Stack(
                              fit: StackFit.expand,
                              children: [
                                ColoredBox(
                                  color: palette.page,
                                  child: ConversationList(
                                    state: state,
                                    items: _items,
                                    highlight: _highlight,
                                    onActivate: _activate,
                                    emptyText: state.searchQuery != null ? 'No matches' : 'No conversations',
                                  ),
                                ),
                                SlideLayer(
                                  onDismiss: () => state.showBundle(null),
                                  child: bundle == null
                                      ? null
                                      : ColoredBox(
                                          color: palette.page,
                                          child: ConversationList(
                                            key: ValueKey('bundle:${bundle.name}'),
                                            state: state,
                                            items: _bundleItems,
                                            highlight: _bundleHighlight,
                                            onActivate: _activate,
                                            emptyText: 'Nothing left in ${bundle.title}',
                                          ),
                                        ),
                                ),
                                SlideLayer(
                                  onDismiss: state.close,
                                  child: opened == null
                                      ? null
                                      : Reader(key: _reader, state: state, conversation: opened),
                                ),
                                _ToastView(toast: state.toast),
                              ],
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  Widget _toolbar(BuildContext context) {
    final palette = context.palette;
    final opened = state.opened;
    final bundle = state.openBundle;
    final List<Widget> leading;
    final List<Widget> trailing;
    if (opened != null) {
      leading = [HoverIconButton(icon: Icons.chevron_left_rounded, tooltip: 'Back (Esc)', iconSize: 22, onPressed: _back)];
      trailing = [
        if (opened.waiting)
          HoverIconButton(icon: Icons.task_alt_rounded, tooltip: 'Done (no longer waiting)', color: palette.waiting, onPressed: () => state.resolveWaiting(opened)),
        HoverIconButton(icon: Icons.archive_outlined, tooltip: 'Archive (E)', onPressed: () => state.archive(opened)),
        HoverIconButton(icon: Icons.delete_outline_rounded, tooltip: 'Trash (⌫)', onPressed: () => state.trash(opened)),
        HoverIconButton(
          icon: opened.unread ? Icons.mark_email_read_outlined : Icons.mark_email_unread_outlined,
          tooltip: opened.unread ? 'Mark Read (U)' : 'Mark Unread (U)',
          onPressed: () => state.setRead(opened, read: opened.unread),
        ),
        HoverIconButton(
          icon: opened.starred ? Icons.star_rounded : Icons.star_border_rounded,
          tooltip: opened.starred ? 'Unstar (S)' : 'Star (S)',
          color: opened.starred ? palette.star : null,
          onPressed: () => state.setStarred(opened, starred: !opened.starred),
        ),
        const SizedBox(width: 6),
        HoverIconButton(icon: Icons.reply_rounded, tooltip: 'Reply (R)', onPressed: () => _reader.currentState?.reply()),
        HoverIconButton(icon: Icons.reply_all_rounded, tooltip: 'Reply All (A)', onPressed: () => _reader.currentState?.reply(all: true)),
      ];
    } else {
      final unread = state.conversations.where((c) => c.unread).length;
      final title = bundle?.title ?? _title;
      final subtitle = state.isLoading && state.conversations.isEmpty
          ? 'Loading…'
          : bundle != null
              ? '${state.bundleItems(bundle).length} conversations'
              : (unread > 0 ? '$unread unread' : '');
      leading = [
        if (bundle != null || state.searchQuery != null)
          HoverIconButton(icon: Icons.chevron_left_rounded, tooltip: 'Back (Esc)', iconSize: 22, onPressed: _back),
        const SizedBox(width: 6),
        Flexible(
          child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: uiText(TextSize.headline, palette.text, weight: FontWeight.w700)),
        ),
        const SizedBox(width: 10),
        Text(subtitle, style: uiText(TextSize.footnote, palette.muted)),
        if (state.isLoading) ...[
          const SizedBox(width: 10),
          SizedBox(width: 12, height: 12, child: CircularProgressIndicator(strokeWidth: 1.5, color: palette.faint)),
        ],
      ];
      trailing = [
        _SearchField(controller: _search, focus: _searchFocus, onChanged: state.search, onEscape: _back),
        const SizedBox(width: 6),
        HoverIconButton(icon: Icons.refresh_rounded, tooltip: 'Refresh (⌘R)', onPressed: state.refresh),
      ];
    }
    return WindowDragArea(
      child: SizedBox(
        height: 52,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 14),
          child: Row(children: [
            Expanded(child: Row(children: leading)),
            ...trailing,
          ]),
        ),
      ),
    );
  }
}

class _SearchField extends StatelessWidget {
  const _SearchField({required this.controller, required this.focus, required this.onChanged, required this.onEscape});

  final TextEditingController controller;
  final FocusNode focus;
  final ValueChanged<String> onChanged;
  final VoidCallback onEscape;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    return SizedBox(
      width: 240,
      height: 30,
      child: CallbackShortcuts(
        bindings: {const SingleActivator(LogicalKeyboardKey.escape): onEscape},
        child: TextField(
          controller: controller,
          focusNode: focus,
          onChanged: onChanged,
          cursorColor: palette.accent,
          style: uiText(TextSize.body, palette.text),
          decoration: InputDecoration(
            isDense: true,
            filled: true,
            fillColor: palette.chip,
            hintText: 'Search  ⌘F',
            hintStyle: uiText(TextSize.body, palette.faint),
            prefixIcon: Icon(Icons.search_rounded, size: 16, color: palette.muted),
            prefixIconConstraints: const BoxConstraints(minWidth: 30),
            contentPadding: const EdgeInsets.symmetric(vertical: 8),
            border: OutlineInputBorder(borderRadius: BorderRadius.circular(Corner.control), borderSide: BorderSide.none),
          ),
        ),
      ),
    );
  }
}

class _ToastView extends StatelessWidget {
  const _ToastView({required this.toast});

  final Toast? toast;

  @override
  Widget build(BuildContext context) {
    final palette = context.palette;
    final toast = this.toast;
    return Positioned(
      left: 0,
      right: 0,
      bottom: 22,
      child: IgnorePointer(
        child: Center(
          child: AnimatedSwitcher(
            duration: Motion.brief,
            switchInCurve: houseCurve,
            transitionBuilder: (child, animation) => FadeTransition(
              opacity: animation,
              child: SlideTransition(position: Tween(begin: const Offset(0, 0.4), end: Offset.zero).animate(animation), child: child),
            ),
            child: toast == null
                ? const SizedBox.shrink()
                : Container(
                    key: ValueKey(toast),
                    constraints: const BoxConstraints(maxWidth: 560),
                    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 9),
                    decoration: BoxDecoration(
                      color: toast.error ? palette.danger : const Color(0xE6202022),
                      borderRadius: BorderRadius.circular(Corner.panel),
                      boxShadow: Shadows.floating,
                    ),
                    child: Text(toast.text, style: uiText(TextSize.body, const Color(0xFFFFFFFF), weight: FontWeight.w500)),
                  ),
          ),
        ),
      ),
    );
  }
}
