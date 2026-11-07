import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../backend/models.dart';
import '../platform.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';

/// Someone a [PeopleCompletion] offers: an address, and the picture a chat publishes for them.
class Suggestion {
  const Suggestion(this.address, {this.photoUrl});

  final Address address;
  final String? photoUrl;
}

/// People suggested under a text field as it is typed in, as the native app's recipient fields
/// complete (`RecipientCompletion`): a list floating under the field, ↑/↓ to move through it,
/// Return or Tab (or a click) to take one, Esc to close it. Keys go on to the field and the
/// window when the list is closed. With the field empty (people offered before any typing)
/// nothing is highlighted until ↓, so Return still does what the window does with it.
class PeopleCompletion extends StatefulWidget {
  const PeopleCompletion({
    super.key,
    required this.state,
    required this.controller,
    required this.focusNode,
    required this.suggest,
    required this.onPick,
    this.revision = 0,
    required this.child,
  });

  /// For the people's pictures.
  final MailState state;
  final TextEditingController controller;
  final FocusNode focusNode;

  /// The people for the field's text, best first. Asked again on every edit; an answer that
  /// arrives after a newer question is dropped.
  final Future<List<Suggestion>> Function(String text) suggest;

  /// Takes the chosen person, usually by rewriting the field.
  final ValueChanged<Suggestion> onPick;

  /// Changed when what [suggest] answers changes without an edit (people loaded later).
  final int revision;

  /// The field.
  final Widget child;

  @override
  State<PeopleCompletion> createState() => _PeopleCompletionState();
}

class _PeopleCompletionState extends State<PeopleCompletion> {
  static const _rowHeight = 44.0;
  static const _shown = 6;

  final _overlay = OverlayPortalController(debugLabel: 'People');
  final _link = LayerLink();
  List<Suggestion> _people = const [];

  /// The highlighted row; -1 for none.
  int _highlight = 0;

  /// The latest question; answers to older ones are dropped.
  int _asked = 0;
  String? _lastText;

  /// Esc closed the list; the next edit opens it again.
  bool _dismissed = false;

  @override
  void initState() {
    super.initState();
    widget.controller.addListener(_edited);
    widget.focusNode.addListener(_sync);
    _ask();
  }

  @override
  void didUpdateWidget(PeopleCompletion old) {
    super.didUpdateWidget(old);
    if (old.controller != widget.controller) {
      old.controller.removeListener(_edited);
      widget.controller.addListener(_edited);
    }
    if (old.focusNode != widget.focusNode) {
      old.focusNode.removeListener(_sync);
      widget.focusNode.addListener(_sync);
    }
    if (old.revision != widget.revision) _ask();
  }

  @override
  void dispose() {
    widget.controller.removeListener(_edited);
    widget.focusNode.removeListener(_sync);
    super.dispose();
  }

  void _edited() {
    if (widget.controller.text == _lastText) return;
    _dismissed = false;
    _ask();
  }

  Future<void> _ask() async {
    final text = _lastText = widget.controller.text;
    final asked = ++_asked;
    final people = await widget.suggest(text);
    if (!mounted || asked != _asked) return;
    setState(() {
      _people = people;
      _highlight = text.trim().isEmpty ? -1 : 0;
    });
    _sync();
  }

  bool get _open => widget.focusNode.hasFocus && _people.isNotEmpty && !_dismissed;

  /// Shows or hides the list to match [_open].
  void _sync() {
    if (!mounted) return;
    if (_open) {
      _overlay.show();
    } else if (_overlay.isShowing) {
      _overlay.hide();
    }
  }

  void _pick(Suggestion person) {
    setState(() => _people = const []);
    _sync();
    widget.onPick(person);
    widget.focusNode.requestFocus();
  }

  KeyEventResult _key(FocusNode _, KeyEvent event) {
    if (!_open || event is KeyUpEvent) return KeyEventResult.ignored;
    final key = event.logicalKey;
    if (key == LogicalKeyboardKey.arrowDown || key == LogicalKeyboardKey.arrowUp) {
      final step = key == LogicalKeyboardKey.arrowDown ? 1 : -1;
      setState(() => _highlight = (_highlight + step).clamp(0, _people.length - 1));
      return KeyEventResult.handled;
    }
    if (event is KeyRepeatEvent) return KeyEventResult.ignored;
    if (key == LogicalKeyboardKey.enter || key == LogicalKeyboardKey.numpadEnter || key == LogicalKeyboardKey.tab) {
      // ⌘Return (Ctrl+Enter off the Mac) still sends; ⇧Tab still moves back.
      if (_highlight < 0 || Host.commandPressed || HardwareKeyboard.instance.isShiftPressed) {
        return KeyEventResult.ignored;
      }
      _pick(_people[_highlight]);
      return KeyEventResult.handled;
    }
    if (key == LogicalKeyboardKey.escape) {
      setState(() => _dismissed = true);
      _sync();
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  @override
  Widget build(BuildContext context) => Focus(
    canRequestFocus: false,
    skipTraversal: true,
    onKeyEvent: _key,
    child: CompositedTransformTarget(
      link: _link,
      child: OverlayPortal(controller: _overlay, overlayChildBuilder: _list, child: widget.child),
    ),
  );

  Widget _list(BuildContext context) {
    final colors = context.colors;
    final width = (this.context.findRenderObject() as RenderBox?)?.size.width ?? 300;
    return CompositedTransformFollower(
      link: _link,
      targetAnchor: Alignment.bottomLeft,
      offset: const Offset(-8, 4),
      child: Align(
        alignment: Alignment.topLeft,
        // Clicking a row must not take the focus from the field, as clicking outside one does.
        child: TextFieldTapRegion(
          child: Container(
            width: width + 16,
            constraints: const BoxConstraints(maxHeight: _rowHeight * _shown + 12),
            decoration: ShapeDecoration(
              color: colors.background,
              shape: RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(10),
                side: BorderSide(color: colors.separator, width: 0.5),
              ),
              shadows: [BoxShadow(color: MacosColors.black.withValues(alpha: 0.2), blurRadius: 18, offset: const Offset(0, 6))],
            ),
            // The pictures arrive as the app loads them.
            child: ListenableBuilder(
              listenable: widget.state,
              builder: (context, _) => ListView.builder(
                shrinkWrap: true,
                padding: const EdgeInsets.all(6),
                itemCount: _people.length,
                itemExtent: _rowHeight,
                itemBuilder: (context, i) => _row(context, _people[i], i == _highlight),
              ),
            ),
          ),
        ),
      ),
    );
  }

  Widget _row(BuildContext context, Suggestion person, bool highlighted) {
    final colors = context.colors;
    final text = context.text;
    final address = person.address;
    final email = address.email.toLowerCase();
    widget.state.requestAvatars([(email, false)]);
    final named = address.name != null && address.name!.isNotEmpty && address.name != address.email;
    final foreground = highlighted ? colors.onAccentFill : null;
    return Hoverable(
      selected: highlighted,
      selectedColor: colors.accentFill,
      radius: 6,
      onTap: () => _pick(person),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8),
        child: Row(children: [
          PersonAvatar(address: address, photo: widget.state.avatar(email), photoUrl: person.photoUrl, size: 26),
          const SizedBox(width: 10),
          Expanded(
            child: Column(mainAxisAlignment: MainAxisAlignment.center, crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(
                address.displayName,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: text.body.copyWith(color: foreground ?? colors.label),
              ),
              if (named)
                Text(
                  address.email,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: text.subheadline.copyWith(color: foreground ?? colors.secondaryLabel),
                ),
            ]),
          ),
        ]),
      ),
    );
  }
}
