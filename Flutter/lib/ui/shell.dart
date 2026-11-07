import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart' hide Sidebar;

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

/// The window as islands on macOS 26's Liquid Glass, after Dia: the navigation sits on the
/// window's glass at the leading edge (the sidebar when there is room, else a rail), the list and
/// the conversation are rounded islands beside it. Wide windows show the two islands side by
/// side; narrow ones one, the conversation's sliding over the list's. Bundles slide over the list
/// within its island and leave it mounted underneath.
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
  final _composeKey = GlobalKey(debugLabel: 'Compose pane');

  /// Floating compose island offset from the bottom-right corner and its size.
  Offset _floatOffset = Offset.zero;
  double _floatWidth = 500.0;
  double _floatHeight = 560.0;

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
    state.addListener(_offerSignIn);
    state.addListener(_checkFrameRateMenu);
    _window.setMethodCallHandler((call) async {
      switch (call.method) {
        case 'settings':
          _settings();
        case 'toggleFrameRate':
          state.toggleFrameRate();
      }
    });
  }

  /// View ▸ Show Frame Rate (⌥⌘P) asks the window channel to toggle `flutter.showFrameRate`;
  /// the menu item's check mark follows the setting, whoever changes it.
  bool _frameRateShown = false;

  void _checkFrameRateMenu() {
    final shown = state.showFrameRate;
    if (shown == _frameRateShown) return;
    _frameRateShown = shown;
    _window.invokeMethod<void>('frameRate', shown);
  }

  /// The last toast the engine raised: each one shows once, as a notice floating over the bottom
  /// of the window until [_toastTimer] hides it.
  Toast? _shownToast;
  Toast? _toast;
  Timer? _toastTimer;

  void _showToast() {
    final toast = state.toast;
    if (toast == null || identical(toast, _shownToast)) return;
    _shownToast = toast;
    _toastTimer?.cancel();
    setState(() => _toast = toast);
    _toastTimer = Timer(Duration(seconds: toast.error ? 6 : 3), () {
      if (mounted) setState(() => _toast = null);
    });
  }

  /// Keys reach the window again once the field that had focus goes away (a closed composer or
  /// compose draft), unless a sheet above it (Settings, New chat) holds the keys.
  void _keepFocus() {
    final primary = FocusManager.instance.primaryFocus;
    if (primary != null && primary is! FocusScopeNode) return;
    if (!mounted || !(ModalRoute.isCurrentOf(context) ?? true)) return;
    Future.microtask(() => mounted ? _focus.requestFocus() : null);
  }

  void _compose() => state.composeNew();

  /// The app menu's Settings… (⌘,) asks the window channel; once, over the main window only.
  static const _window = MethodChannel('mailstream/window');

  void _settings() {
    if (mounted && (ModalRoute.isCurrentOf(context) ?? true)) openSettings(context, state);
  }

  /// With no account signed in, Settings opens by itself once the account list is read, where
  /// the person signs in with their OAuth client, as the app opens its Accounts window.
  void _offerSignIn() {
    if (!state.accountsLoaded) return;
    state.removeListener(_offerSignIn);
    if (state.accounts.isEmpty) WidgetsBinding.instance.addPostFrameCallback((_) => _settings());
  }

  @override
  void dispose() {
    FocusManager.instance.removeListener(_keepFocus);
    _window.setMethodCallHandler(null);
    state.removeListener(_showToast);
    state.removeListener(_offerSignIn);
    state.removeListener(_checkFrameRateMenu);
    _toastTimer?.cancel();
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
      } else if (key == LogicalKeyboardKey.keyW && event is KeyDownEvent) {
        // Closes the open conversation or chat space, as its close button does; the compose
        // column takes ⌘W itself while it has focus.
        if (state.chat.active ? state.chat.opened == null : state.opened == null) return KeyEventResult.ignored;
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
      case LogicalKeyboardKey.keyF when reading:
        _reader.currentState?.forward();
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
            // The window's glass shows around the islands.
            child: Stack(
              children: [
                Positioned.fill(child: LayoutBuilder(builder: _layout)),
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 24,
                  child: Center(
                    child: AnimatedSwitcher(
                      duration: Motion.short,
                      child: _toast == null ? const SizedBox.shrink() : _ToastView(key: ObjectKey(_toast), toast: _toast!),
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

  /// Breakpoints: the full sidebar from 1200 dp (1440 dp while the compose column is docked), a
  /// rail below. The list's island fills the rest; an opened conversation splits it when there is
  /// room, and the compose column sits at the trailing edge by default (resizable, swappable,
  /// floatable, or stashed into the bottom-right capsule).
  Widget _layout(BuildContext context, BoxConstraints constraints) {
    final width = constraints.maxWidth;
    final height = constraints.maxHeight;
    final hasCompose = state.composes.isNotEmpty;
    final stashedCompose = hasCompose && state.composeStashed;
    final floatingCompose = hasCompose && !state.composeStashed && state.composeFloating;
    final dockedCompose = hasCompose && !state.composeStashed && !state.composeFloating;

    final sidebarThreshold = dockedCompose ? 1440.0 : 1200.0;
    final drawer = width >= sidebarThreshold || _drawerOpen;
    final navWidth = drawer ? Sidebar.width : Rail.width;
    final contentWidth = (width - navWidth - Panes.gap).clamp(320.0, double.infinity);

    final maxCompose = (contentWidth - 360.0 - Panes.gap).clamp(320.0, 760.0);
    final minCompose = 340.0.clamp(320.0, maxCompose);
    final composeWidth = state.composeWidth.clamp(minCompose, maxCompose);
    final mailWidth = dockedCompose ? (contentWidth - composeWidth - Panes.gap).clamp(320.0, double.infinity) : contentWidth;

    _twoPane = dockedCompose ? mailWidth >= 700.0 : width >= 1000.0;

    final navigation = _onChat(() => drawer
        ? Sidebar(
            state: state,
            onAddAccount: _settings,
            onCollapse: width >= sidebarThreshold ? null : () => setState(() => _drawerOpen = false),
          )
        : Rail(state: state, onMenu: () => setState(() => _drawerOpen = true)));
    final chat = state.chat;
    final listPane = Island(
      color: context.colors.tray,
      child: chat.active ? _onChat(() => ChatListPane(state: state)) : _listPane(context),
    );
    final opened = chat.active ? null : state.opened;
    final space = chat.active ? chat.opened : null;
    final detail = space != null
        ? Island(child: _chatPane(space))
        : opened == null
        ? null
        : Island(child: _detailPane(context, opened));
    final onDismiss = chat.active ? chat.close : state.close;
    // With the compose column docked the list's width follows the window, not the column, so
    // resizing the column moves only the edge being dragged.
    final listWidth = dockedCompose ? (contentWidth * 0.3).clamp(300.0, 460.0) : (width * 0.36).clamp(400.0, 520.0);
    final Widget mailContent = _twoPane
        ? SplitDetail(pageId: space ?? opened?.id, list: listPane, listWidth: listWidth, onDismiss: onDismiss, detail: detail)
        : Stack(
            fit: StackFit.expand,
            clipBehavior: Clip.none,
            children: [listPane, SlideLayer(pageId: space ?? opened?.id, onDismiss: onDismiss, child: detail)],
          );

    final composePane = hasCompose && !stashedCompose
        ? ComposePane(
            key: _composeKey,
            state: state,
            onUnfocus: () => _focus.requestFocus(),
            onDragFloating: floatingCompose
                ? (delta) => setState(() {
                    _floatOffset = Offset(
                      (_floatOffset.dx - delta.dx).clamp(0.0, (width - _floatWidth - 32).clamp(0.0, double.infinity)),
                      (_floatOffset.dy - delta.dy).clamp(0.0, (height - _floatHeight - 32).clamp(0.0, double.infinity)),
                    );
                  })
                : null,
          )
        : null;

    // Resizes the docked column from a drag of [dx] on its leading or trailing edge, within [widest].
    void resize(double dx, {required bool trailing, double? widest}) {
      final current = state.composeWidth.clamp(minCompose, maxCompose);
      state.setComposeWidth((current + (trailing ? dx : -dx)).clamp(minCompose, widest ?? maxCompose));
    }

    final Widget content;
    if (!dockedCompose || composePane == null) {
      content = mailContent;
    } else if (state.composeSwapped && _twoPane && detail != null) {
      // Between the list and the conversation, it widens only as far as keeps the conversation
      // beside the list: past that the layout would fold into one pane under the pointer.
      final widest = math.min(contentWidth - Panes.gap - 700.0, contentWidth - listWidth - 2 * Panes.gap - 360.0);
      content = Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          SizedBox(width: listWidth, child: listPane),
          const SizedBox(width: Panes.gap),
          SizedBox(width: composeWidth, child: composePane),
          _ResizeGap(onDrag: (dx) => resize(dx, trailing: true, widest: math.max(math.min(widest, maxCompose), composeWidth))),
          Expanded(child: detail),
        ],
      );
    } else if (state.composeSwapped) {
      content = Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          SizedBox(width: composeWidth, child: composePane),
          _ResizeGap(onDrag: (dx) => resize(dx, trailing: true)),
          Expanded(child: mailContent),
        ],
      );
    } else {
      content = Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Expanded(child: mailContent),
          _ResizeGap(onDrag: (dx) => resize(dx, trailing: false)),
          SizedBox(width: composeWidth, child: composePane),
        ],
      );
    }

    return Stack(
      fit: StackFit.expand,
      children: [
        // The glass between the islands moves the window, as a title bar does.
        const WindowDragArea(child: SizedBox.expand()),
        Row(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            navigation,
            Expanded(
              child: Padding(padding: const EdgeInsets.fromLTRB(0, Panes.gap, Panes.gap, Panes.gap), child: content),
            ),
          ],
        ),
        if (floatingCompose && composePane != null)
          Positioned(
            right: (16 + _floatOffset.dx).clamp(8.0, (width - _floatWidth - 8).clamp(8.0, double.infinity)),
            bottom: (16 + _floatOffset.dy).clamp(8.0, (height - _floatHeight - 8).clamp(8.0, double.infinity)),
            width: _floatWidth.clamp(360.0, (width - 32).clamp(360.0, 760.0)),
            height: _floatHeight.clamp(360.0, (height - 32).clamp(360.0, 760.0)),
            child: Stack(
              children: [
                Positioned.fill(child: composePane),
                Positioned(
                  left: 0,
                  top: 0,
                  width: 14,
                  height: 14,
                  child: MouseRegion(
                    cursor: SystemMouseCursors.resizeUpLeftDownRight,
                    child: GestureDetector(
                      behavior: HitTestBehavior.translucent,
                      onPanUpdate: (d) => setState(() {
                        _floatWidth = (_floatWidth - d.delta.dx).clamp(380.0, (width - 32).clamp(380.0, 760.0));
                        _floatHeight = (_floatHeight - d.delta.dy).clamp(380.0, (height - 32).clamp(380.0, 760.0));
                      }),
                    ),
                  ),
                ),
              ],
            ),
          ),
        if (stashedCompose)
          Positioned(
            right: 16,
            bottom: 16,
            child: StashedComposeBar(state: state),
          ),
      ],
    );
  }

  /// The list's island: its title row and search field over the rows; an open bundle slides over
  /// the rows on a page of its own.
  Widget _listPane(BuildContext context) {
    final bundle = state.openBundle;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WindowDragArea(
          child: _ListHeader(
            title: bundle?.title ?? _title,
            subtitle: _subtitle,
            loading: state.isLoading,
            onBack: bundle != null || state.searchQuery != null ? _back : null,
            onRefresh: state.refresh,
            onCompose: _compose,
          ),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
          child: _SearchField(controller: _search, focus: _searchFocus, onChanged: state.search, onEscape: _back),
        ),
        const Hairline(),
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
                  pageId: bundle?.name,
                  onDismiss: () => state.showBundle(null),
                  child: bundle == null
                      ? null
                      : _Page(
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
              ],
            ),
          ),
        ),
      ],
    );
  }

  /// The open chat space, a fresh reader (and composer) per space, fading through like mail.
  Widget _chatPane(SpaceKey space) => AnimatedSwitcher(
    duration: Motion.medium,
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

  /// The conversation's island: the message scrolls under its toolbar, glass capsules floating
  /// over the top of it.
  Widget _detailPane(BuildContext context, Conversation opened) => Stack(
    children: [
      Positioned.fill(
        child: AnimatedSwitcher(
          duration: Motion.medium,
          switchInCurve: Motion.curve,
          // Keep WebKit's native viewport unscaled: its asynchronously composited page
          // can drift relative to the Flutter card during a scale transition (MS-82).
          // Fading preserves the transition without changing native clip geometry.
          transitionBuilder: (child, animation) => FadeTransition(
            opacity: animation,
            child: child,
          ),
          child: KeyedSubtree(
            key: ValueKey(opened.id),
            child: Reader(key: _reader, state: state, conversation: opened, topInset: toolbarHeight),
          ),
        ),
      ),
      Positioned(
        top: 0,
        left: 0,
        right: 0,
        height: toolbarHeight,
        child: WindowDragArea(child: _readerToolbar(context, opened)),
      ),
    ],
  );

  /// The band at the top of a conversation's island that its toolbar floats in.
  static const toolbarHeight = 56.0;

  /// The toolbar's groups are capsules of the system's glass, as in macOS 26 Mail.
  Widget _readerToolbar(BuildContext context, Conversation opened) {
    final colors = context.colors;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 10),
      child: Row(children: [
        GlassCapsule(
          child: ToolButton(
            tooltip: _twoPane ? 'Close (Esc or ⌘W)' : 'Back (Esc or ⌘W)',
            icon: _twoPane ? CupertinoIcons.xmark : CupertinoIcons.chevron_left,
            onPressed: _back,
          ),
        ),
        const Spacer(),
        GlassCapsule(
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            if (opened.waiting)
              ToolButton(
                tooltip: 'Resolve (D)',
                icon: CupertinoIcons.checkmark_circle,
                color: colors.waiting,
                onPressed: () => state.setWaiting(opened, waiting: false),
              ),
            ToolButton(tooltip: 'Archive (E)', icon: CupertinoIcons.archivebox, onPressed: () => _act(state.archive)),
            ToolButton(tooltip: 'Delete (⌫)', icon: CupertinoIcons.trash, onPressed: () => _act(state.trash)),
            ToolButton(
              tooltip: opened.unread ? 'Mark as read (U)' : 'Mark as unread (U)',
              icon: opened.unread ? CupertinoIcons.envelope_open : CupertinoIcons.envelope_badge,
              onPressed: () => state.setRead(opened, read: opened.unread),
            ),
            ToolButton(
              tooltip: opened.starred ? 'Unstar (S)' : 'Star (S)',
              icon: opened.starred ? CupertinoIcons.star_fill : CupertinoIcons.star,
              color: opened.starred ? starColor : null,
              onPressed: () => state.setStarred(opened, starred: !opened.starred),
            ),
          ]),
        ),
        const SizedBox(width: 8),
        GlassCapsule(
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            ToolButton(tooltip: 'Reply (R)', icon: CupertinoIcons.arrowshape_turn_up_left, onPressed: () => _reader.currentState?.reply()),
            ToolButton(
              tooltip: 'Reply all (A)',
              icon: CupertinoIcons.arrowshape_turn_up_left_2,
              onPressed: () => _reader.currentState?.reply(all: true),
            ),
            ToolButton(
              tooltip: 'Forward (F)',
              icon: CupertinoIcons.arrowshape_turn_up_right,
              onPressed: () => _reader.currentState?.forward(),
            ),
          ]),
        ),
      ]),
    );
  }
}

/// A page over the list's rows, inside its island (an open bundle): the island's surface, a soft
/// shadow on its leading edge.
class _Page extends StatelessWidget {
  const _Page({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => DecoratedBox(
    decoration: BoxDecoration(
      boxShadow: [BoxShadow(color: MacosColors.black.withValues(alpha: 0.1), blurRadius: 12, offset: const Offset(-2, 0))],
    ),
    child: ColoredBox(color: context.colors.tray, child: child),
  );
}

/// The list's title row: the mailbox and its count, a spinner while it loads, refresh and compose
/// on a glass capsule.
class _ListHeader extends StatelessWidget {
  const _ListHeader({
    required this.title,
    required this.subtitle,
    required this.loading,
    required this.onBack,
    required this.onRefresh,
    required this.onCompose,
  });

  static const height = 56.0;

  final String title;
  final String? subtitle;
  final bool loading;
  final VoidCallback? onBack;
  final VoidCallback onRefresh;
  final VoidCallback onCompose;

  @override
  Widget build(BuildContext context) {
    final subtitle = this.subtitle;
    return SizedBox(
      height: height,
      child: Row(children: [
        const SizedBox(width: 10),
        if (onBack != null) ...[
          GlassCapsule(child: ToolButton(tooltip: 'Back (Esc)', icon: CupertinoIcons.chevron_left, onPressed: onBack)),
          const SizedBox(width: 10),
        ] else
          const SizedBox(width: 6),
        Expanded(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(children: [
                Flexible(
                  child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: context.text.title3.copyWith(fontWeight: FontWeight.w700)),
                ),
                if (loading) const Padding(padding: EdgeInsets.only(left: 8), child: Spinner(radius: 6)),
              ]),
              if (subtitle != null && subtitle.isNotEmpty)
                Text(subtitle, maxLines: 1, style: context.text.subheadline.copyWith(color: context.colors.secondaryLabel)),
            ],
          ),
        ),
        GlassCapsule(
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            ToolButton(tooltip: 'Refresh (⌘R)', icon: CupertinoIcons.arrow_clockwise, onPressed: onRefresh),
            ToolButton(tooltip: 'New message (⌘N)', icon: CupertinoIcons.square_pencil, onPressed: onCompose),
          ]),
        ),
        const SizedBox(width: 10),
      ]),
    );
  }
}

/// macos_ui's search field; Esc leaves the search.
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
  Widget build(BuildContext context) => CallbackShortcuts(
    bindings: {const SingleActivator(LogicalKeyboardKey.escape): onEscape},
    child: MacosSearchField(
      controller: controller,
      focusNode: focus,
      onChanged: onChanged,
      placeholder: 'Search',
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 6),
    ),
  );
}

/// A notice from the engine, as the Mac floats them: a dark rounded panel (red for an error)
/// with a soft shadow.
class _ToastView extends StatelessWidget {
  const _ToastView({super.key, required this.toast});

  final Toast toast;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final error = toast.error;
    return Container(
      constraints: const BoxConstraints(maxWidth: 460),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
      decoration: BoxDecoration(
        color: error ? colors.redFill : colors.toast,
        borderRadius: BorderRadius.circular(12),
        boxShadow: [BoxShadow(color: MacosColors.black.withValues(alpha: 0.2), blurRadius: 16, offset: const Offset(0, 6))],
      ),
      child: Row(mainAxisSize: MainAxisSize.min, children: [
        if (error) ...[
          MacosIcon(CupertinoIcons.exclamationmark_triangle_fill, size: 15, color: colors.onRedFill),
          const SizedBox(width: 8),
        ],
        Flexible(child: Text(toast.text, style: context.text.body.copyWith(color: error ? colors.onRedFill : colors.onToast))),
      ]),
    );
  }
}

/// The gap between a docked compose island and its neighbor: drags horizontally to resize the
/// compose column and shows a pill handle on hover.
class _ResizeGap extends StatefulWidget {
  const _ResizeGap({required this.onDrag});

  final ValueChanged<double> onDrag;

  @override
  State<_ResizeGap> createState() => _ResizeGapState();
}

class _ResizeGapState extends State<_ResizeGap> {
  bool _hovered = false;
  bool _dragging = false;

  @override
  Widget build(BuildContext context) {
    final active = _hovered || _dragging;
    return MouseRegion(
      cursor: SystemMouseCursors.resizeLeftRight,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        onHorizontalDragStart: (_) => setState(() => _dragging = true),
        onHorizontalDragUpdate: (details) => widget.onDrag(details.delta.dx),
        onHorizontalDragEnd: (_) => setState(() => _dragging = false),
        onHorizontalDragCancel: () => setState(() => _dragging = false),
        child: SizedBox(
          width: Panes.gap,
          child: Center(
            child: AnimatedOpacity(
              duration: Motion.quick,
              opacity: active ? 1 : 0,
              child: Container(
                width: 3,
                height: 36,
                decoration: BoxDecoration(
                  color: context.colors.border,
                  borderRadius: BorderRadius.circular(1.5),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
