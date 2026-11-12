import 'dart:io';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:path/path.dart' as p;

import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart';
import '../platform.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'controls.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';
import 'people.dart';

/// The window's files (`MainFlutterWindow` on the Mac, `windows/runner/window_channels.cpp` on
/// Windows): `accept` turns drops on the window on or off, `editing` says whether the compose
/// column has focus, `pick` shows an open panel and `pasted` answers the files on the clipboard
/// (copied image pixels written to a PNG first); the window calls back with `hover` and `drop`,
/// and on the Mac with `paste` from the Edit menu.
const _files = MethodChannel('mailstream/files');

enum _Field {
  to('To'),
  cc('Cc'),
  bcc('Bcc');

  const _Field(this.label);

  final String label;
}

/// [person] as a recipient: `Name <address>`, the name quoted when it holds a separator.
String formatRecipient(engine.Address person) {
  final name = person.name?.replaceAll('"', '').trim();
  if (name == null || name.isEmpty || name == person.email) return person.email;
  return RegExp(r'[,;<>@()\[\]:\\.]').hasMatch(name) ? '"$name" <${person.email}>' : '$name <${person.email}>';
}

/// The compose column (or floating island): hosts one or more [ComposeDraft] tabs so the person
/// can read and browse other conversations on the left while writing.
class ComposePane extends StatefulWidget {
  const ComposePane({super.key, required this.state, required this.onUnfocus, this.onDragFloating});

  final MailState state;
  final VoidCallback onUnfocus;
  final ValueChanged<Offset>? onDragFloating;

  @override
  State<ComposePane> createState() => _ComposePaneState();
}

class _ComposePaneState extends State<ComposePane> {
  final Map<int, GlobalKey<_ComposeDraftViewState>> _draftKeys = {};
  bool _dropping = false;

  GlobalKey<_ComposeDraftViewState> _keyFor(int id) =>
      _draftKeys.putIfAbsent(id, () => GlobalKey<_ComposeDraftViewState>(debugLabel: 'ComposeDraft:$id'));

  _ComposeDraftViewState? get _activeState {
    final active = widget.state.activeCompose;
    return active == null ? null : _draftKeys[active.id]?.currentState;
  }

  @override
  void initState() {
    super.initState();
    _files.setMethodCallHandler(_onFiles);
    _files.invokeMethod<void>('accept', true);
  }

  @override
  void didUpdateWidget(ComposePane oldWidget) {
    super.didUpdateWidget(oldWidget);
    final activeIds = {for (final d in widget.state.composes) d.id};
    _draftKeys.removeWhere((id, _) => !activeIds.contains(id));
  }

  @override
  void dispose() {
    _files.invokeMethod<void>('accept', false);
    _files.setMethodCallHandler(null);
    super.dispose();
  }

  Future<void> _onFiles(MethodCall call) async {
    if (!mounted) return;
    final active = widget.state.activeCompose;
    switch (call.method) {
      case 'hover':
        setState(() => _dropping = call.arguments == true && active != null && !active.sending);
      case 'drop':
        _activeState?.attach((call.arguments as List).cast<String>());
      case 'paste':
        _activeState?.paste();
    }
  }

  @override
  Widget build(BuildContext context) {
    final composes = widget.state.composes;
    if (composes.isEmpty) return const SizedBox.shrink();
    final activeIndex = widget.state.activeComposeIndex.clamp(0, composes.length - 1);
    return Focus(
      onFocusChange: (focused) => _files.invokeMethod<void>('editing', focused),
      child: Island(
        child: Stack(
          children: [
            Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (composes.length > 1) ...[
                  _ComposeTabBar(
                    state: widget.state,
                    activeIndex: activeIndex,
                    onCloseTab: (draft) => _keyFor(draft.id).currentState?.requestClose(),
                  ),
                  const Hairline(),
                ],
                Expanded(
                  child: IndexedStack(
                    index: activeIndex,
                    children: [
                      for (final draft in composes)
                        _ComposeDraftView(
                          key: _keyFor(draft.id),
                          state: widget.state,
                          draft: draft,
                          onUnfocus: widget.onUnfocus,
                          onDragFloating: widget.onDragFloating,
                        ),
                    ],
                  ),
                ),
              ],
            ),
            if (_dropping) Positioned.fill(child: IgnorePointer(child: _dropTarget(context))),
          ],
        ),
      ),
    );
  }

  Widget _dropTarget(BuildContext context) {
    final colors = context.colors;
    return Container(
      margin: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        color: colors.accentFill.withValues(alpha: 0.92),
        borderRadius: BorderRadius.circular(Panes.corner - 4),
        border: Border.all(color: colors.accent, width: 2),
      ),
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          MacosIcon(CupertinoIcons.paperclip, size: 36, color: colors.onAccentFill),
          const SizedBox(height: 12),
          Text('Drop files to attach', style: context.text.title3.copyWith(color: colors.onAccentFill)),
        ],
      ),
    );
  }
}

class _ComposeTabBar extends StatelessWidget {
  const _ComposeTabBar({required this.state, required this.activeIndex, required this.onCloseTab});

  final MailState state;
  final int activeIndex;
  final ValueChanged<ComposeDraft> onCloseTab;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return SizedBox(
      height: 36,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        child: Row(
          children: [
            Expanded(
              child: ListView.separated(
                scrollDirection: Axis.horizontal,
                itemCount: state.composes.length,
                separatorBuilder: (_, _) => const SizedBox(width: 4),
                itemBuilder: (context, index) {
                  final draft = state.composes[index];
                  final selected = index == activeIndex;
                  final icon = switch (draft.kind) {
                    ComposeKind.newMessage => CupertinoIcons.square_pencil,
                    ComposeKind.reply => CupertinoIcons.arrowshape_turn_up_left,
                    ComposeKind.forward => CupertinoIcons.arrowshape_turn_up_right,
                  };
                  return Hoverable(
                    selected: selected,
                    selectedColor: colors.selection,
                    radius: 6,
                    onTap: () => state.selectCompose(index),
                    child: Padding(
                      padding: const EdgeInsets.only(left: 8, right: 4),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          MacosIcon(icon, size: 12, color: selected ? colors.label : colors.secondaryLabel),
                          const SizedBox(width: 5),
                          ConstrainedBox(
                            constraints: const BoxConstraints(maxWidth: 140),
                            child: Text(
                              draft.tabTitle,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: context.text.subheadline.copyWith(
                                color: selected ? colors.label : colors.secondaryLabel,
                                fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                              ),
                            ),
                          ),
                          const SizedBox(width: 4),
                          MailIconButton(
                            icon: CupertinoIcons.xmark,
                            tooltip: 'Close tab',
                            size: 16,
                            iconSize: 10,
                            onPressed: () => onCloseTab(draft),
                          ),
                        ],
                      ),
                    ),
                  );
                },
              ),
            ),
            const SizedBox(width: 4),
            ToolButton(tooltip: 'New draft tab (${Host.shortcut('N')})', icon: CupertinoIcons.plus, onPressed: state.composeNew),
          ],
        ),
      ),
    );
  }
}

class _ComposeDraftView extends StatefulWidget {
  const _ComposeDraftView({super.key, required this.state, required this.draft, required this.onUnfocus, this.onDragFloating});

  final MailState state;
  final ComposeDraft draft;
  final VoidCallback onUnfocus;
  final ValueChanged<Offset>? onDragFloating;

  @override
  State<_ComposeDraftView> createState() => _ComposeDraftViewState();
}

class _ComposeDraftViewState extends State<_ComposeDraftView> {
  late final _fields = {
    _Field.to: TextEditingController(text: widget.draft.to),
    _Field.cc: TextEditingController(text: widget.draft.cc),
    _Field.bcc: TextEditingController(text: widget.draft.bcc),
  };
  final _focus = {for (final field in _Field.values) field: FocusNode()};
  late final _subject = TextEditingController(text: widget.draft.subject);
  final _subjectFocus = FocusNode();
  late final _body = TextEditingController(text: widget.draft.body);
  final _bodyFocus = FocusNode();

  /// Fields the last Send refused.
  final Set<_Field> _invalid = {};

  ComposeDraft get d => widget.draft;

  @override
  void initState() {
    super.initState();
    d.from ??= widget.state.preferredComposeAccount;
    _fields[_Field.to]!.addListener(() {
      final prev = d.to;
      d.to = _fields[_Field.to]!.text;
      if ((prev.isEmpty != d.to.isEmpty || widget.state.composes.length > 1) && d.subject.isEmpty) {
        widget.state.notifyComposeChanged();
      }
    });
    _fields[_Field.cc]!.addListener(() => d.cc = _fields[_Field.cc]!.text);
    _fields[_Field.bcc]!.addListener(() => d.bcc = _fields[_Field.bcc]!.text);
    _subject.addListener(() {
      d.subject = _subject.text;
      if (widget.state.composes.length > 1 || widget.state.composeStashed) {
        widget.state.notifyComposeChanged();
      }
    });
    _body.addListener(() => d.body = _body.text);

    if (d.initialBodySelection != null) {
      final offset = d.initialBodySelection!.clamp(0, _body.text.length);
      _body.selection = TextSelection.collapsed(offset: offset);
      d.initialBodySelection = null;
    }
    if (d.focusBodyInitially) {
      d.focusBodyInitially = false;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _bodyFocus.requestFocus();
      });
    } else if (d.to.isEmpty) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _focus[_Field.to]!.requestFocus();
      });
    }
    for (final path in d.attachments) {
      if (!d.attachmentSizes.containsKey(path)) {
        File(path).length().then((size) {
          if (mounted) setState(() => d.attachmentSizes[path] = size);
        }, onError: (_) {});
      }
    }
  }

  @override
  void dispose() {
    for (final controller in [..._fields.values, _subject, _body]) {
      controller.dispose();
    }
    for (final node in [..._focus.values, _subjectFocus, _bodyFocus]) {
      node.dispose();
    }
    super.dispose();
  }

  Future<void> _pick() async => attach(await _files.invokeListMethod<String>('pick') ?? const []);

  /// The Mac's Edit > Paste, and Ctrl+V off the Mac, enter here; dispatch through EditableText's
  /// overridable action so ordinary text retains the editor's selection, caret and undo behavior.
  void paste() {
    final focused = primaryFocus?.context;
    if (focused != null) Actions.maybeInvoke(focused, const PasteTextIntent(SelectionChangedCause.keyboard));
  }

  /// Called by an overridable paste action, including when an EditableText owns focus.
  Future<bool> _pasteAttachments() async {
    try {
      final files = await _files.invokeListMethod<String>('pasted') ?? const [];
      if (!mounted) return true;
      if (files.isEmpty) return false;
      attach(files);
      return true;
    } on PlatformException catch (error) {
      if (mounted) _fail(error.message ?? 'Could not paste attachments.');
      return true;
    }
  }

  /// Adds the files not attached yet; nothing changes while sending.
  void attach(List<String> paths) {
    if (d.sending || !mounted) return;
    final added = paths.where((p) => !d.attachments.contains(p)).toList();
    if (added.isEmpty) return;
    setState(() => d.attachments.addAll(added));
    for (final path in added) {
      File(path).length().then((size) {
        if (mounted) setState(() => d.attachmentSizes[path] = size);
      }, onError: (_) {});
    }
  }

  void _close() => widget.state.closeCompose(d.id);

  /// Closes at once when nothing would be lost; otherwise shows the draft and asks in the bar at
  /// its bottom.
  void requestClose() {
    if (d.sending) return;
    if (!d.hasContent) return _close();
    widget.state.selectCompose(widget.state.composes.indexOf(d));
    setState(() => d.confirmingDiscard = true);
  }

  /// Esc takes back the discard question first; closes an empty draft; or leaves a written draft
  /// open and returns keyboard focus to the mail list/reader so the user can browse other mail.
  void _escape() {
    if (d.confirmingDiscard) return setState(() => d.confirmingDiscard = false);
    if (!d.hasContent) {
      _close();
      widget.onUnfocus();
      return;
    }
    FocusManager.instance.primaryFocus?.unfocus();
    widget.onUnfocus();
  }

  void _fail(String message) => setState(() => d.error = message);

  Future<void> _send() async {
    if (d.sending) return;
    final from = d.from;
    if (from == null) return _fail('Add an account in Mailstream to send from.');
    _invalid.clear();
    final lists = <_Field, List<String>>{};
    for (final field in _Field.values) {
      final parsed = engine.parseRecipients(text: _fields[field]!.text);
      if (parsed.invalid case final invalid?) {
        _invalid.add(field);
        if (field == _Field.bcc) d.showBcc = true;
        _focus[field]!.requestFocus();
        return _fail('${field.label}: “$invalid” is not an email address.');
      }
      lists[field] = parsed.recipients;
    }
    if (d.replyTo == null ? lists[_Field.to]!.isEmpty : lists.values.every((recipients) => recipients.isEmpty)) {
      _invalid.add(_Field.to);
      _focus[_Field.to]!.requestFocus();
      return _fail('Add at least one recipient.');
    }
    await widget.state.sendCompose(
      d,
      engine.Draft(
        from: from,
        to: lists[_Field.to]!,
        cc: lists[_Field.cc]!,
        bcc: lists[_Field.bcc]!,
        subject: _subject.text.trim(),
        body: _body.text,
        attachments: List.of(d.attachments),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return Actions(
      actions: {PasteTextIntent: AttachmentPasteAction(_pasteAttachments)},
      child: CallbackShortcuts(
        bindings: {
          Host.command(LogicalKeyboardKey.enter): _send,
          Host.command(LogicalKeyboardKey.keyW): requestClose,
          // The Mac's Edit menu takes ⌘V before Flutter and calls `paste` through the channel.
          if (!Host.mac) Host.command(LogicalKeyboardKey.keyV): paste,
          const SingleActivator(LogicalKeyboardKey.escape): _escape,
        },
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _titleBar(context),
            if (d.from != null)
              _row(
                context,
                label: 'From',
                child: d.replyTo == null ? _fromPicker(context) : Text(d.from!, style: text.body),
              ),
            _recipients(context, _Field.to),
            _recipients(
              context,
              _Field.cc,
              trailing: d.showBcc
                  ? null
                  : MailButton(
                      label: 'Bcc',
                      onPressed: d.sending
                          ? null
                          : () {
                              setState(() => d.showBcc = true);
                              _focus[_Field.bcc]!.requestFocus();
                            },
                    ),
            ),
            if (d.showBcc) _recipients(context, _Field.bcc),
            _row(
              context,
              child: _bareField(
                controller: _subject,
                focusNode: _subjectFocus,
                enabled: !d.sending,
                style: text.title3,
                placeholder: 'Subject',
              ),
            ),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(20, 12, 20, 12),
                child: _bareField(
                  controller: _body,
                  focusNode: _bodyFocus,
                  enabled: !d.sending,
                  expands: true,
                  style: text.body.copyWith(height: 1.55),
                  placeholder: d.replyTo != null ? 'Write a reply (the original is quoted below it)' : 'Write your message…',
                ),
              ),
            ),
            if (d.attachments.isNotEmpty)
              Padding(
                padding: const EdgeInsets.fromLTRB(20, 0, 20, 12),
                child: Wrap(spacing: 8, runSpacing: 8, children: [for (final path in d.attachments) _attachmentChip(context, path)]),
              ),
            if (d.sending || d.error != null)
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 10),
                decoration: BoxDecoration(
                  border: Border(top: BorderSide(color: colors.separator, width: 0.5)),
                ),
                child: Text(
                  d.sending ? 'Sending…' : d.error!,
                  style: text.subheadline.copyWith(color: d.sending ? colors.secondaryLabel : colors.red),
                ),
              ),
            if (d.confirmingDiscard) _discardBar(context),
          ],
        ),
      ),
    );
  }

  /// A file's name and size, and a button removing it.
  Widget _attachmentChip(BuildContext context, String path) {
    final size = d.attachmentSizes[path];
    final name = p.basename(path);
    return MacosTooltip(
      message: path,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 300),
        child: Pill(
          label: size == null ? name : '$name  ·  ${fileSize(size)}',
          leading: MacosIcon(CupertinoIcons.paperclip, size: 13, color: context.colors.secondaryLabel),
          onDelete: d.sending
              ? null
              : () => setState(() {
                  d.attachments.remove(path);
                  d.attachmentSizes.remove(path);
                }),
        ),
      ),
    );
  }

  /// A text field without a box, as Mail's header lines and body are.
  Widget _bareField({
    required TextEditingController controller,
    FocusNode? focusNode,
    required bool enabled,
    bool autofocus = false,
    bool expands = false,
    required TextStyle style,
    String? placeholder,
    ValueChanged<String>? onChanged,
  }) => MailTextField(
    controller: controller,
    focusNode: focusNode,
    enabled: enabled,
    autofocus: autofocus,
    expands: expands,
    maxLines: expands ? null : 1,
    textAlignVertical: expands ? TextAlignVertical.top : null,
    keyboardType: expands ? TextInputType.multiline : null,
    style: style,
    placeholder: placeholder,
    bare: true,
    onChanged: onChanged,
  );

  /// The draft's toolbar, as every detail pane's: its close capsule, the title ("New Message",
  /// "Reply"), its tools and Send.
  Widget _titleBar(BuildContext context) {
    final state = widget.state;
    final floating = state.composeFloating;
    final titleWidget = Text(
      d.replyAll ? 'Reply All' : d.kind.title,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: context.roles.paneTitle,
    );
    final draggableTitle = floating && widget.onDragFloating != null
        ? MouseRegion(
            cursor: SystemMouseCursors.move,
            child: GestureDetector(
              behavior: HitTestBehavior.translucent,
              onPanUpdate: (details) => widget.onDragFloating!(details.delta),
              child: Padding(padding: const EdgeInsets.symmetric(vertical: 6), child: titleWidget),
            ),
          )
        : WindowDragArea(
            child: Padding(padding: const EdgeInsets.symmetric(vertical: 6), child: titleWidget),
          );

    return DetailToolbar(
      leading: GlassCapsule(
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            ToolButton(tooltip: 'Close draft (${Host.shortcut('W')})', icon: CupertinoIcons.xmark, onPressed: d.sending ? null : requestClose),
            ToolButton(
              tooltip: 'Minimize to bottom-right',
              icon: CupertinoIcons.minus,
              onPressed: () {
                state.setComposeStashed(true);
                widget.onUnfocus();
              },
            ),
          ],
        ),
      ),
      title: draggableTitle,
      actions: [
        GlassCapsule(
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (state.composes.length == 1)
                ToolButton(tooltip: 'New message (${Host.shortcut('N')})', icon: CupertinoIcons.plus, onPressed: state.composeNew),
              if (!floating)
                ToolButton(
                  tooltip: state.composeSwapped ? 'Move column to right edge' : 'Swap column position',
                  icon: CupertinoIcons.arrow_left_right,
                  onPressed: state.toggleComposeSwapped,
                ),
              ToolButton(
                tooltip: floating ? 'Dock as column' : 'Float over window',
                icon: floating ? CupertinoIcons.sidebar_right : CupertinoIcons.macwindow,
                onPressed: state.toggleComposeFloating,
              ),
              ToolButton(
                tooltip: 'Attach files (or drop them on the message)',
                icon: CupertinoIcons.paperclip,
                onPressed: d.sending ? null : _pick,
              ),
            ],
          ),
        ),
        MacosTooltip(
          message: Host.shortcut('Return'),
          child: MailButton(
            label: d.sending ? 'Sending' : 'Send',
            icon: CupertinoIcons.paperplane_fill,
            kind: MailButtonKind.primary,
            size: MailControlSize.large,
            busy: d.sending,
            onPressed: _send,
          ),
        ),
      ],
    );
  }

  /// One header line over a hairline: a right-aligned label, then the field.
  Widget _row(BuildContext context, {String? label, bool invalid = false, required Widget child, Widget? trailing}) {
    final colors = context.colors;
    return Container(
      height: 44,
      margin: const EdgeInsets.only(left: 20),
      padding: const EdgeInsets.only(right: 16),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: colors.separator, width: 0.5)),
      ),
      child: Row(
        children: [
          if (label != null) ...[
            SizedBox(
              width: 48,
              child: Text(
                '$label:',
                textAlign: TextAlign.end,
                style: context.text.body.copyWith(color: invalid ? colors.red : colors.secondaryLabel),
              ),
            ),
            const SizedBox(width: 10),
          ],
          Expanded(child: child),
          ?trailing,
        ],
      ),
    );
  }

  /// A recipient field, completing the address being typed (the one after the last comma) to
  /// the people `contacts.search` finds, as the native compose window does.
  Widget _recipients(BuildContext context, _Field field, {Widget? trailing}) => _row(
    context,
    label: field.label,
    invalid: _invalid.contains(field),
    trailing: trailing,
    child: PeopleCompletion(
      state: widget.state,
      controller: _fields[field]!,
      focusNode: _focus[field]!,
      suggest: _suggest,
      onPick: (person) => _complete(field, person.address),
      child: _bareField(
        controller: _fields[field]!,
        focusNode: _focus[field],
        enabled: !d.sending,
        autofocus: false,
        style: context.text.body,
        onChanged: (_) {
          if (_invalid.contains(field)) setState(() => _invalid.remove(field));
        },
      ),
    ),
  );

  /// The people for the address being typed in a field, without those already in To, Cc or Bcc.
  Future<List<Suggestion>> _suggest(String text) async {
    final typed = engine.splitRecipients(text: text).last.trim();
    if (typed.isEmpty || d.sending) return const [];
    final people = await widget.state.searchContacts(typed);
    final chosen = {
      for (final field in _Field.values)
        for (final part in engine.splitRecipients(text: _fields[field]!.text))
          if (part.trim().isNotEmpty && part.trim() != typed) parseAddress(part).email.toLowerCase(),
    };
    return [
      for (final person in people)
        if (!chosen.contains(person.email.toLowerCase())) Suggestion(person),
    ];
  }

  /// Puts [person] in place of the address being typed in [field], ready for the next one.
  void _complete(_Field field, engine.Address person) {
    final controller = _fields[field]!;
    final typing = engine.splitRecipients(text: controller.text).last;
    final before = controller.text.substring(0, controller.text.length - typing.length);
    final text = '${before.isEmpty || before.endsWith(' ') ? before : '$before '}${formatRecipient(person)}, ';
    controller.value = TextEditingValue(
      text: text,
      selection: TextSelection.collapsed(offset: text.length),
    );
    if (_invalid.contains(field)) setState(() => _invalid.remove(field));
  }

  Widget _fromPicker(BuildContext context) {
    final accounts = widget.state.accounts;
    String title(String id) {
      final account = widget.state.account(id);
      return account == null || account.title == id ? id : '${account.title} <$id>';
    }

    if (accounts.length < 2) return Text(title(d.from!), style: context.text.body);
    return Align(
      alignment: Alignment.centerLeft,
      child: MailMenuButton<String>(
        value: d.from,
        onChanged: d.sending ? null : (id) => setState(() => d.from = id),
        items: [for (final account in accounts) MailMenuItem(account.id, title(account.id))],
      ),
    );
  }

  Widget _discardBar(BuildContext context) {
    final colors = context.colors;
    return Container(
      color: colors.fill,
      padding: const EdgeInsets.fromLTRB(20, 8, 12, 8),
      child: Row(
        children: [
          Expanded(child: Text('Discard this message?', style: context.text.body)),
          MailButton(label: 'Keep Editing', onPressed: () => setState(() => d.confirmingDiscard = false)),
          const SizedBox(width: Space.s8),
          MailButton(label: 'Discard', kind: MailButtonKind.destructive, onPressed: _close),
        ],
      ),
    );
  }
}

/// The minimized capsule at the bottom-right corner when the compose column is stashed.
class StashedComposeBar extends StatelessWidget {
  const StashedComposeBar({super.key, required this.state});

  final MailState state;

  @override
  Widget build(BuildContext context) {
    final active = state.activeCompose;
    if (active == null) return const SizedBox.shrink();
    final colors = context.colors;
    final count = state.composes.length;
    return RaisedCapsule(
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Hoverable(
            radius: 14,
            onTap: () => state.setComposeStashed(false),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  MacosIcon(CupertinoIcons.square_pencil, size: 15, color: colors.accent),
                  const SizedBox(width: 8),
                  ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 220),
                    child: Text(
                      active.tabTitle,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: context.text.body.copyWith(fontWeight: FontWeight.w600),
                    ),
                  ),
                  if (count > 1) ...[
                    const SizedBox(width: 8),
                    Pill(label: '$count drafts', background: colors.accentFill, foreground: colors.onAccentFill),
                  ],
                  const SizedBox(width: 6),
                  MacosIcon(CupertinoIcons.sidebar_right, size: 14, color: colors.secondaryLabel),
                ],
              ),
            ),
          ),
          ToolButton(tooltip: 'New draft (${Host.shortcut('N')})', icon: CupertinoIcons.plus, onPressed: state.composeNew),
        ],
      ),
    );
  }
}

/// EditableText exposes an overridable paste action. Preserve its original text action for
/// text-only clipboards, including selection replacement and caret placement.
class AttachmentPasteAction extends Action<PasteTextIntent> {
  AttachmentPasteAction(this.attach);
  final Future<bool> Function() attach;

  @override
  Object? invoke(PasteTextIntent intent) {
    final textPaste = callingAction;
    () async {
      if (!await attach()) textPaste?.invoke(intent);
    }();
    return null;
  }
}
