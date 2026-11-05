import 'package:flutter/cupertino.dart' show CupertinoIcons;
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
import 'glass.dart';
import 'reader.dart';
import 'settings.dart';
import 'sidebar.dart';
import 'slide_layer.dart';

/// The window, laid out as macOS 26 Mail: the navigation on a Liquid Glass panel at the leading
/// edge (the sidebar when there is room, else a rail), the content on the window's surface. Wide
/// windows show the list and the conversation side by side, split by a hairline; narrow ones show
/// one, the conversation sliding over the list. Bundles slide over the list either way and leave
/// it mounted underneath.
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
              // The window's surface is painted around the navigation's glass panel, which stays
              // open (`GlassPanel`).
              backgroundColor: Colors.transparent,
              body: LayoutBuilder(builder: (context, constraints) => _layout(context, constraints.maxWidth)),
            ),
          ),
        );
      },
    );
  }

  /// Breakpoints: the full sidebar from 1200 dp, a rail below, either on a glass panel. The list
  /// fills the content area; from 1000 dp an opened conversation splits it, list-detail, the
  /// list narrowing as the conversation slides in beside it. Narrower windows slide the
  /// conversation over the list.
  Widget _layout(BuildContext context, double width) {
    final drawer = width >= 1200 || _drawerOpen;
    _twoPane = width >= 1000;
    final navigation = _onChat(() => drawer
        ? Sidebar(state: state, onCollapse: width >= 1200 ? null : () => setState(() => _drawerOpen = false))
        : Rail(state: state, onMenu: () => setState(() => _drawerOpen = true)));
    final chat = state.chat;
    final listPane = chat.active ? _onChat(() => ChatListPane(state: state)) : _listPane(context);
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
        GlassPanel(child: navigation),
        Expanded(child: ColoredBox(color: context.colors.surface, child: content)),
      ],
    );
  }

  /// The list's toolbar: the title row, the search field, the progress line.
  static const _listHeaderHeight = _ListHeader.height + 40.0 + 2;

  /// The list scrolls under its toolbar, a frosted glass bar.
  Widget _listPane(BuildContext context) {
    final bundle = state.openBundle;
    return Stack(
      fit: StackFit.expand,
      children: [
        ClipRect(
          child: Stack(
            fit: StackFit.expand,
            children: [
              ConversationList(
                state: state,
                items: _items,
                highlight: _highlight,
                onActivate: _activate,
                topInset: _listHeaderHeight,
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
                        topInset: _listHeaderHeight,
                        emptyText: 'Nothing left in ${bundle.title}',
                      ),
              ),
            ],
          ),
        ),
        Positioned(
          top: 0,
          left: 0,
          right: 0,
          height: _listHeaderHeight,
          child: GlassBar(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                WindowDragArea(
                  child: _ListHeader(
                    title: bundle?.title ?? _title,
                    subtitle: _subtitle,
                    onBack: bundle != null || state.searchQuery != null ? _back : null,
                    onRefresh: state.refresh,
                    onCompose: _compose,
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
                  child: _SearchField(controller: _search, focus: _searchFocus, onChanged: state.search, onEscape: _back),
                ),
                SizedBox(height: 2, child: state.isLoading ? const LinearProgressIndicator() : null),
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
      WindowDragArea(child: SizedBox(height: 52, child: _readerToolbar(context, opened))),
      Expanded(
        child: AnimatedSwitcher(
          duration: Durations.medium2,
          switchInCurve: Motion.curve,
          // Fade through: the old conversation fades out as the next one scales in.
          transitionBuilder: (child, animation) => FadeTransition(
            opacity: animation,
            child: ScaleTransition(scale: Tween(begin: 0.98, end: 1.0).animate(animation), child: child),
          ),
          child: KeyedSubtree(key: ValueKey(opened.id), child: Reader(key: _reader, state: state, conversation: opened)),
        ),
      ),
    ],
  );

  /// The toolbar's groups are glass capsules, as in macOS 26 Mail.
  Widget _readerToolbar(BuildContext context, Conversation opened) {
    final colors = context.colors;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12),
      child: Row(children: [
        GlassCapsule(
          child: IconButton(
            tooltip: _twoPane ? 'Close (Esc)' : 'Back (Esc)',
            icon: Icon(_twoPane ? CupertinoIcons.xmark : CupertinoIcons.chevron_left),
            onPressed: _back,
          ),
        ),
        const Spacer(),
        GlassCapsule(
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            if (opened.waiting)
              IconButton(
                tooltip: 'Resolve (D)',
                icon: Icon(CupertinoIcons.checkmark_circle, color: colors.tertiary),
                onPressed: () => state.setWaiting(opened, waiting: false),
              ),
            IconButton(tooltip: 'Archive (E)', icon: const Icon(CupertinoIcons.archivebox), onPressed: () => _act(state.archive)),
            IconButton(tooltip: 'Delete (⌫)', icon: const Icon(CupertinoIcons.trash), onPressed: () => _act(state.trash)),
            IconButton(
              tooltip: opened.unread ? 'Mark as read (U)' : 'Mark as unread (U)',
              icon: Icon(opened.unread ? CupertinoIcons.envelope_open : CupertinoIcons.envelope_badge),
              onPressed: () => state.setRead(opened, read: opened.unread),
            ),
            IconButton(
              tooltip: opened.starred ? 'Unstar (S)' : 'Star (S)',
              icon: Icon(opened.starred ? CupertinoIcons.star_fill : CupertinoIcons.star, color: opened.starred ? starColor : null),
              onPressed: () => state.setStarred(opened, starred: !opened.starred),
            ),
          ]),
        ),
        const SizedBox(width: 8),
        GlassCapsule(
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            IconButton(tooltip: 'Reply (R)', icon: const Icon(CupertinoIcons.arrowshape_turn_up_left), onPressed: () => _reader.currentState?.reply()),
            IconButton(
              tooltip: 'Reply all (A)',
              icon: const Icon(CupertinoIcons.arrowshape_turn_up_left_2),
              onPressed: () => _reader.currentState?.reply(all: true),
            ),
          ]),
        ),
      ]),
    );
  }
}

/// The conversation beside the list: the window's surface, a hairline on its leading edge.
class _Pane extends StatelessWidget {
  const _Pane({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => DecoratedBox(
    decoration: BoxDecoration(
      color: context.colors.surface,
      border: Border(left: BorderSide(color: context.colors.outlineVariant, width: 0.5)),
    ),
    child: child,
  );
}

/// The list toolbar's title row: the mailbox and its count, as Mail titles its toolbar, with
/// refresh and compose on a glass capsule.
class _ListHeader extends StatelessWidget {
  const _ListHeader({
    required this.title,
    required this.subtitle,
    required this.onBack,
    required this.onRefresh,
    required this.onCompose,
  });

  static const height = 56.0;

  final String title;
  final String? subtitle;
  final VoidCallback? onBack;
  final VoidCallback onRefresh;
  final VoidCallback onCompose;

  @override
  Widget build(BuildContext context) {
    final subtitle = this.subtitle;
    return SizedBox(
      height: height,
      child: Row(children: [
        const SizedBox(width: 12),
        if (onBack != null) ...[
          GlassCapsule(child: IconButton(tooltip: 'Back (Esc)', icon: const Icon(CupertinoIcons.chevron_left), onPressed: onBack)),
          const SizedBox(width: 10),
        ] else
          const SizedBox(width: 6),
        Expanded(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.titleMedium?.copyWith(fontWeight: FontWeight.w700)),
              if (subtitle != null && subtitle.isNotEmpty)
                Text(subtitle, maxLines: 1, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
            ],
          ),
        ),
        GlassCapsule(
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            IconButton(tooltip: 'Refresh (⌘R)', icon: const Icon(CupertinoIcons.arrow_clockwise), onPressed: onRefresh),
            IconButton(tooltip: 'New message (⌘N)', icon: const Icon(CupertinoIcons.square_pencil), onPressed: onCompose),
          ]),
        ),
        const SizedBox(width: 12),
      ]),
    );
  }
}

/// The Mac's search field: a gray capsule with a magnifying glass.
class _SearchField extends StatelessWidget {
  const _SearchField({
    required this.controller,
    required this.focus,
    required this.onChanged,
    required this.onEscape,
  });

  final TextEditingController controller;
  final FocusNode focus;
  final ValueChanged<String> onChanged;
  final VoidCallback onEscape;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return CallbackShortcuts(
      bindings: {const SingleActivator(LogicalKeyboardKey.escape): onEscape},
      child: SearchBar(
        controller: controller,
        focusNode: focus,
        onChanged: onChanged,
        hintText: 'Search',
        constraints: const BoxConstraints(minHeight: 32, maxHeight: 32),
        elevation: const WidgetStatePropertyAll(0),
        backgroundColor: WidgetStatePropertyAll(colors.onSurface.withValues(alpha: colors.brightness == Brightness.light ? 0.06 : 0.1)),
        overlayColor: const WidgetStatePropertyAll(Colors.transparent),
        textStyle: WidgetStatePropertyAll(context.text.bodyMedium),
        hintStyle: WidgetStatePropertyAll(context.text.bodyMedium?.copyWith(color: colors.onSurfaceVariant)),
        padding: const WidgetStatePropertyAll(EdgeInsets.symmetric(horizontal: 10)),
        leading: Icon(CupertinoIcons.search, size: 15, color: colors.onSurfaceVariant),
      ),
    );
  }
}
