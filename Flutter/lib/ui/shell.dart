import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../backend/models.dart';
import '../state/chat_state.dart';
import '../state/mail_state.dart';
import 'chat.dart';
import 'common.dart';
import 'compose.dart';
import 'conversation_list.dart';
import 'design.dart';
import 'reader.dart';
import 'settings.dart';
import 'sidebar.dart';
import 'slide_layer.dart';

/// The window, laid out by Material's large-screen guidance: navigation on the leading edge
/// (a drawer when there is room, else a rail), then panes on the surface container. Wide windows
/// show the list-detail canonical layout, list and conversation side by side; narrow ones show
/// one pane, the conversation sliding over the list. Bundles slide over the list either way and
/// leave it mounted underneath.
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

  /// The rail's menu button opens the full drawer.
  bool _drawerOpen = false;

  /// Whether the window is wide enough for list and conversation side by side.
  bool _twoPane = false;

  MailState get state => widget.state;

  /// What the list and the open bundle show, for drawing and for the keyboard.
  List<ListItem> _items = const [];
  List<ListItem> _bundleItems = const [];

  @override
  void initState() {
    super.initState();
    FocusManager.instance.addListener(_keepFocus);
    state.addListener(_showToast);
    _window.setMethodCallHandler((call) async {
      if (call.method == 'settings') _settings();
    });
  }

  /// The last toast shown as a snackbar: each one shows once.
  Toast? _shownToast;

  void _showToast() {
    final toast = state.toast;
    if (toast == null || identical(toast, _shownToast)) return;
    _shownToast = toast;
    final colors = Theme.of(context).colorScheme;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(
        content: Text(toast.text, style: toast.error ? TextStyle(color: colors.onErrorContainer) : null),
        backgroundColor: toast.error ? colors.errorContainer : null,
        duration: Duration(seconds: toast.error ? 6 : 3),
      ));
  }

  /// Keys reach the window again once the field that had focus goes away (a closed composer),
  /// unless a window above it (the compose window) holds the keys.
  void _keepFocus() {
    final primary = FocusManager.instance.primaryFocus;
    if (primary != null && primary is! FocusScopeNode) return;
    if (!mounted || !(ModalRoute.isCurrentOf(context) ?? true)) return;
    Future.microtask(() => mounted ? _focus.requestFocus() : null);
  }

  void _compose() => openCompose(context, state);

  /// The app menu's Settings… (⌘,) asks the window channel; once, over the main window only.
  static const _window = MethodChannel('mailstream/window');

  void _settings() {
    if (mounted && (ModalRoute.isCurrentOf(context) ?? true)) openSettings(context, state);
  }

  @override
  void dispose() {
    FocusManager.instance.removeListener(_keepFocus);
    _window.setMethodCallHandler(null);
    state.removeListener(_showToast);
    _focus.dispose();
    _searchFocus.dispose();
    _search.dispose();
    _highlight.dispose();
    _bundleHighlight.dispose();
    super.dispose();
  }

  ListHighlight get _activeHighlight => state.openBundle != null ? _bundleHighlight : _highlight;
  List<ListItem> get _activeItems => state.openBundle != null ? _bundleItems : _items;

  void _activate(ListItem item) {
    _focus.requestFocus();
    switch (item) {
      case RowItem(:final conversation):
        _activeHighlight.id = conversation.id;
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
    if (state.chat.active) {
      state.chat.close();
      _focus.requestFocus();
      return;
    }
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
    for (final item in _activeItems) {
      if (item is RowItem && item.conversation.id == _activeHighlight.id) return item.conversation;
    }
    return null;
  }

  /// The row `step` away from the highlight.
  ListItem? _neighbor(int step) {
    final items = _activeItems.where((i) => i is! SectionItem).toList();
    if (items.isEmpty) return null;
    final index = items.indexWhere((i) => itemId(i) == _activeHighlight.id);
    final next = index < 0 ? (step > 0 ? 0 : items.length - 1) : (index + step).clamp(0, items.length - 1);
    return items[next];
  }

  /// Moves the highlight; with the conversation beside the list, the next one opens too.
  void _move(int step) {
    final item = _neighbor(step);
    if (item == null) return;
    _activeHighlight.id = itemId(item);
    if (item is RowItem) {
      if (_twoPane && state.opened != null) {
        state.open(item.conversation);
      } else {
        state.prefetch(item.conversation);
      }
    }
  }

  void _openHighlighted() {
    for (final item in _activeItems) {
      if (itemId(item) == _activeHighlight.id) return _activate(item);
    }
  }

  /// After the conversation leaves the list, the highlight moves to its neighbor, which opens
  /// in its place when the conversation was open beside the list.
  void _act(void Function(Conversation) action) {
    final target = _target;
    if (target == null) return;
    final reading = state.opened != null;
    if (!reading || _twoPane) {
      final next = _neighbor(1);
      final previous = _neighbor(-1);
      final replacement = next != null && itemId(next) != target.id ? next : previous;
      action(target);
      if (replacement == null || itemId(replacement) == target.id) return;
      _activeHighlight.id = itemId(replacement);
      if (reading && replacement is RowItem && state.opened == null) state.open(replacement.conversation);
      return;
    }
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
        state.chat.active ? state.chat.refresh() : state.refresh();
      } else if (key == LogicalKeyboardKey.keyN) {
        _compose();
      } else if (key == LogicalKeyboardKey.bracketLeft) {
        _back();
      } else {
        return KeyEventResult.ignored;
      }
      return KeyEventResult.handled;
    }
    if (_editing) return KeyEventResult.ignored;
    if (state.chat.active) {
      if (key != LogicalKeyboardKey.escape) return KeyEventResult.ignored;
      _back();
      return KeyEventResult.handled;
    }
    final reading = state.opened != null;
    final listInView = !reading || _twoPane;
    switch (key) {
      case LogicalKeyboardKey.escape:
        _back();
      case LogicalKeyboardKey.arrowDown || LogicalKeyboardKey.keyJ when listInView:
        _move(1);
      case LogicalKeyboardKey.arrowUp || LogicalKeyboardKey.keyK when listInView:
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
      case LogicalKeyboardKey.keyD:
        _act((c) {
          if (c.canMarkWaiting) state.setWaiting(c, waiting: !c.waiting);
        });
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
      MailboxSelection(:final mailbox) => mailbox.name,
    };
  }

  String? get _subtitle {
    if (state.searchQuery != null) return null;
    final bundle = state.openBundle;
    if (bundle != null) return '${state.bundleItems(bundle).length} conversations';
    final unread = state.conversations.where((c) => c.unread).length;
    final account = switch (state.selection) {
      MailboxSelection(:final mailbox) => state.account(mailbox.account)?.title,
      UnifiedSelection() => null,
    };
    return [?account, if (unread > 0) '$unread unread'].join(' · ');
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    // Chat's spaces and messages rebuild only the parts that show them (`_onChat`); the window
    // follows chat coming into view and the open space.
    return ListenableBuilder(
      listenable: Listenable.merge([state, state.chat.layout]),
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
            child: Scaffold(
              backgroundColor: colors.surfaceContainer,
              body: LayoutBuilder(builder: (context, constraints) => _layout(context, constraints.maxWidth)),
            ),
          ),
        );
      },
    );
  }

  /// Breakpoints: a drawer from 1200 dp (large), a rail below. The list fills the content area;
  /// from 1000 dp an opened conversation splits it, list-detail, the list narrowing as the
  /// conversation slides in beside it. Narrower windows slide the conversation over the list.
  Widget _layout(BuildContext context, double width) {
    final drawer = width >= 1200 || _drawerOpen;
    _twoPane = width >= 1000;
    final navigation = _onChat(() => drawer
        ? Sidebar(state: state, onCompose: _compose, onCollapse: width >= 1200 ? null : () => setState(() => _drawerOpen = false))
        : Rail(state: state, onCompose: _compose, onMenu: () => setState(() => _drawerOpen = true)));
    final chat = state.chat;
    final listPane = _Pane(child: chat.active ? _onChat(() => ChatListPane(state: state)) : _listPane(context));
    final opened = chat.active ? null : state.opened;
    final space = chat.active ? chat.opened : null;
    final Widget content;
    if (_twoPane) {
      final listWidth = (width * 0.36).clamp(400.0, 520.0);
      content = SplitDetail(
        list: listPane,
        listWidth: listWidth,
        onDismiss: chat.active ? chat.close : state.close,
        detail: space != null
            ? _Pane(child: _chatPane(space))
            : opened == null
            ? null
            : _Pane(child: _detailPane(context, opened)),
      );
    } else {
      content = Stack(
        fit: StackFit.expand,
        children: [
          listPane,
          SlideLayer(
            onDismiss: chat.active ? chat.close : state.close,
            child: space != null
                ? _Pane(child: _chatPane(space))
                : opened == null
                ? null
                : _Pane(child: _detailPane(context, opened)),
          ),
        ],
      );
    }
    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        navigation,
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(0, Panes.margin, Panes.margin, Panes.margin),
            child: content,
          ),
        ),
      ],
    );
  }

  Widget _listPane(BuildContext context) {
    final bundle = state.openBundle;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(10, 10, 10, 0),
            child: _SearchField(controller: _search, focus: _searchFocus, onChanged: state.search, onEscape: _back, onRefresh: state.refresh),
          ),
        ),
        _ListHeader(
          title: bundle?.title ?? _title,
          subtitle: _subtitle,
          onBack: bundle != null || state.searchQuery != null ? _back : null,
        ),
        SizedBox(height: 3, child: state.isLoading ? const LinearProgressIndicator() : null),
        Expanded(
          child: ClipRect(
            child: Stack(
              fit: StackFit.expand,
              children: [
                ConversationList(
                  state: state,
                  items: _items,
                  highlight: _highlight,
                  onActivate: _activate,
                  emptyText: state.searchQuery != null ? 'No matches' : 'No conversations',
                ),
                SlideLayer(
                  onDismiss: () => state.showBundle(null),
                  child: bundle == null
                      ? null
                      : ConversationList(
                          key: ValueKey('bundle:${bundle.name}'),
                          state: state,
                          items: _bundleItems,
                          highlight: _bundleHighlight,
                          onActivate: _activate,
                          emptyText: 'Nothing left in ${bundle.title}',
                        ),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }

  /// The open chat space, a fresh reader (and composer) per space, fading through like mail.
  Widget _chatPane(SpaceKey space) => AnimatedSwitcher(
    duration: Durations.medium2,
    switchInCurve: Motion.curve,
    transitionBuilder: (child, animation) => FadeTransition(
      opacity: animation,
      child: ScaleTransition(scale: Tween(begin: 0.98, end: 1.0).animate(animation), child: child),
    ),
    child: _onChat(
      key: ValueKey(space),
      () => ChatReader(state: state, space: space, twoPane: _twoPane, onBack: _back),
    ),
  );

  /// `build`'s widget, rebuilt as chat's spaces and messages change.
  Widget _onChat(Widget Function() build, {Key? key}) =>
      ListenableBuilder(key: key, listenable: state.chat, builder: (context, _) => build());

  Widget _detailPane(BuildContext context, Conversation opened) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      WindowDragArea(child: SizedBox(height: 54, child: _readerToolbar(context, opened))),
      Expanded(
        child: AnimatedSwitcher(
          duration: Durations.medium2,
          switchInCurve: Motion.curve,
          // Material fade through: the old conversation fades out as the next one scales in.
          transitionBuilder: (child, animation) => FadeTransition(
            opacity: animation,
            child: ScaleTransition(scale: Tween(begin: 0.98, end: 1.0).animate(animation), child: child),
          ),
          child: KeyedSubtree(key: ValueKey(opened.id), child: Reader(key: _reader, state: state, conversation: opened)),
        ),
      ),
    ],
  );

  Widget _readerToolbar(BuildContext context, Conversation opened) {
    final colors = context.colors;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12),
      child: Row(children: [
        IconButton(
          tooltip: _twoPane ? 'Close (Esc)' : 'Back (Esc)',
          icon: Icon(_twoPane ? Icons.close : Icons.arrow_back),
          onPressed: _back,
        ),
        const Spacer(),
        if (opened.waiting)
          IconButton(
            tooltip: 'Resolve (D)',
            icon: Icon(Icons.task_alt, color: colors.tertiary),
            onPressed: () => state.setWaiting(opened, waiting: false),
          ),
        IconButton(tooltip: 'Archive (E)', icon: const Icon(Icons.archive_outlined), onPressed: () => _act(state.archive)),
        IconButton(tooltip: 'Delete (⌫)', icon: const Icon(Icons.delete_outline), onPressed: () => _act(state.trash)),
        IconButton(
          tooltip: opened.unread ? 'Mark as read (U)' : 'Mark as unread (U)',
          icon: Icon(opened.unread ? Icons.drafts_outlined : Icons.mark_email_unread_outlined),
          onPressed: () => state.setRead(opened, read: opened.unread),
        ),
        IconButton(
          tooltip: opened.starred ? 'Unstar (S)' : 'Star (S)',
          icon: Icon(opened.starred ? Icons.star_rounded : Icons.star_outline_rounded, color: opened.starred ? const Color(0xFFF4B400) : null),
          onPressed: () => state.setStarred(opened, starred: !opened.starred),
        ),
        const SizedBox(height: 24, child: VerticalDivider(width: 16)),
        IconButton(tooltip: 'Reply (R)', icon: const Icon(Icons.reply), onPressed: () => _reader.currentState?.reply()),
        IconButton(tooltip: 'Reply all (A)', icon: const Icon(Icons.reply_all), onPressed: () => _reader.currentState?.reply(all: true)),
      ]),
    );
  }
}

/// A pane: a rounded surface on the window's surface container.
class _Pane extends StatelessWidget {
  const _Pane({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => Material(
    color: context.colors.surface,
    borderRadius: BorderRadius.circular(Panes.corner),
    clipBehavior: Clip.antiAlias,
    child: child,
  );
}

class _ListHeader extends StatelessWidget {
  const _ListHeader({required this.title, required this.subtitle, required this.onBack});

  final String title;
  final String? subtitle;
  final VoidCallback? onBack;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: 44,
    child: Row(children: [
      if (onBack != null)
        Padding(padding: const EdgeInsets.only(left: 8), child: IconButton(tooltip: 'Back (Esc)', icon: const Icon(Icons.arrow_back), onPressed: onBack))
      else
        const SizedBox(width: 24),
      const SizedBox(width: 4),
      Flexible(child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.titleLarge)),
      if (subtitle != null && subtitle!.isNotEmpty) ...[
        const SizedBox(width: 10),
        Padding(
          padding: const EdgeInsets.only(top: 3),
          child: Text(subtitle!, style: context.text.labelLarge?.copyWith(color: context.colors.onSurfaceVariant)),
        ),
      ],
      const SizedBox(width: 16),
    ]),
  );
}

/// A Material 3 search bar with refresh at its trailing end.
class _SearchField extends StatelessWidget {
  const _SearchField({
    required this.controller,
    required this.focus,
    required this.onChanged,
    required this.onEscape,
    required this.onRefresh,
  });

  final TextEditingController controller;
  final FocusNode focus;
  final ValueChanged<String> onChanged;
  final VoidCallback onEscape;
  final VoidCallback onRefresh;

  @override
  Widget build(BuildContext context) => CallbackShortcuts(
    bindings: {const SingleActivator(LogicalKeyboardKey.escape): onEscape},
    child: SearchBar(
      controller: controller,
      focusNode: focus,
      onChanged: onChanged,
      hintText: 'Search mail',
      constraints: const BoxConstraints(minHeight: 44, maxHeight: 44),
      elevation: const WidgetStatePropertyAll(0),
      backgroundColor: WidgetStatePropertyAll(context.colors.surfaceContainerHigh),
      padding: const WidgetStatePropertyAll(EdgeInsets.only(left: 16, right: 4)),
      leading: const Icon(Icons.search),
      trailing: [IconButton(tooltip: 'Refresh (⌘R)', icon: const Icon(Icons.refresh), onPressed: onRefresh)],
    ),
  );
}
