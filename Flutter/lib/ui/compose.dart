import 'dart:io';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../backend/models.dart';
import '../state/mail_state.dart';
import 'common.dart';
import 'design.dart';
import 'format.dart';
import 'glass.dart';
import 'people.dart';

/// The window's files (`MainFlutterWindow`): `accept` turns drops on the window on or off,
/// `pick` shows an open panel and `pasted` answers the files on the clipboard; the window calls
/// back with `hover` and `drop`.
const _files = MethodChannel('mailstream/files');

/// Opens the compose window for a new message (⌘N, the Compose button), from the account whose
/// mailbox is shown, else the first. Like the other front ends' compose windows it sends with
/// `mail.send`, and clicking Send is the confirmation the engine asks for.
Future<void> openCompose(BuildContext context, MailState state) {
  final from = switch (state.selection) {
    MailboxSelection(:final mailbox) => mailbox.account,
    UnifiedSelection() => state.accounts.firstOrNull?.id,
  };
  return showSheet<void>(context, (_) => ComposeWindow(state: state, from: from));
}

enum _Field {
  to('To'),
  cc('Cc'),
  bcc('Bcc');

  const _Field(this.label);

  final String label;
}

/// The recipient list split at commas and semicolons outside quotes and angle brackets.
List<String> _splitRecipients(String text) {
  final parts = <String>[];
  var start = 0;
  var quoted = false, angle = false;
  for (var i = 0; i < text.length; i++) {
    switch (text[i]) {
      case '"':
        quoted = !quoted;
      case '<' when !quoted:
        angle = true;
      case '>' when !quoted:
        angle = false;
      case ',' || ';' when !quoted && !angle:
        parts.add(text.substring(start, i));
        start = i + 1;
    }
  }
  parts.add(text.substring(start));
  return parts;
}

bool _validEmail(String email) {
  final at = email.lastIndexOf('@');
  if (at <= 0) return false;
  final domain = email.substring(at + 1);
  return domain.contains('.') && !domain.startsWith('.') && !domain.endsWith('.') && !RegExp(r'[\s<>,;"]').hasMatch(email);
}

/// A field's recipients as written (`Name <address>` or a bare address), or the first one that
/// is not an address.
({List<String> recipients, String? invalid}) _parseRecipients(String text) {
  final recipients = <String>[];
  for (final part in _splitRecipients(text).map((p) => p.trim()).where((p) => p.isNotEmpty)) {
    final angled = RegExp(r'<([^>]*)>\s*$').firstMatch(part);
    if (!_validEmail((angled?.group(1) ?? part).trim())) return (recipients: recipients, invalid: part);
    recipients.add(part);
  }
  return (recipients: recipients, invalid: null);
}

/// [person] as a recipient: `Name <address>`, the name quoted when it holds a separator.
String _recipient(Address person) {
  final name = person.name?.replaceAll('"', '').trim();
  if (name == null || name.isEmpty || name == person.email) return person.email;
  return RegExp(r'[,;<>@()\[\]:\\.]').hasMatch(name) ? '"$name" <${person.email}>' : '$name <${person.email}>';
}

/// A new message: From, To, Cc (Bcc on request), Subject, a plain-text body and attachments
/// (the Attach button, files dropped anywhere on the window or pasted). ⌘Return sends; Esc or ⌘W closes,
/// asking first when something is written. A failed send keeps the window open with the
/// engine's reason underneath.
class ComposeWindow extends StatefulWidget {
  const ComposeWindow({super.key, required this.state, required this.from});

  final MailState state;

  /// The sending account's id; null when there is no account.
  final String? from;

  @override
  State<ComposeWindow> createState() => _ComposeWindowState();
}

class _ComposeWindowState extends State<ComposeWindow> {
  late String? _from = widget.from;
  final _fields = {for (final field in _Field.values) field: TextEditingController()};
  final _focus = {for (final field in _Field.values) field: FocusNode()};
  final _subject = TextEditingController();
  final _body = TextEditingController();
  bool _showBcc = false;
  bool _sending = false;
  bool _confirmingDiscard = false;
  String? _error;

  /// Full paths of the attached files, with their sizes once read.
  final List<String> _attachments = [];
  final Map<String, int> _sizes = {};

  /// Whether files are being dragged over the window.
  bool _dropping = false;

  /// Fields the last Send refused.
  final Set<_Field> _invalid = {};

  @override
  void initState() {
    super.initState();
    _files.setMethodCallHandler(_onFiles);
    _files.invokeMethod<void>('accept', true);
  }

  @override
  void dispose() {
    _files.invokeMethod<void>('accept', false);
    _files.setMethodCallHandler(null);
    for (final controller in [..._fields.values, _subject, _body]) {
      controller.dispose();
    }
    for (final node in _focus.values) {
      node.dispose();
    }
    super.dispose();
  }

  Future<void> _onFiles(MethodCall call) async {
    if (!mounted) return;
    switch (call.method) {
      case 'hover':
        setState(() => _dropping = call.arguments == true && !_sending);
      case 'drop':
        _attach((call.arguments as List).cast<String>());
    }
  }

  Future<void> _pick() async => _attach(await _files.invokeListMethod<String>('pick') ?? const []);

  /// ⌘V: files on the clipboard (copied in Finder) attach, as in the native compose window;
  /// anything else pastes as text where the cursor is.
  Future<void> _paste() async {
    final focused = primaryFocus?.context;
    final files = await _files.invokeListMethod<String>('pasted') ?? const [];
    if (!mounted) return;
    if (files.isNotEmpty) return _attach(files);
    if (focused != null && focused.mounted) {
      Actions.maybeInvoke(focused, const PasteTextIntent(SelectionChangedCause.keyboard));
    }
  }

  /// Adds the files not attached yet; nothing changes while sending.
  void _attach(List<String> paths) {
    if (_sending || !mounted) return;
    final added = paths.where((p) => !_attachments.contains(p)).toList();
    if (added.isEmpty) return;
    setState(() => _attachments.addAll(added));
    for (final path in added) {
      File(path).length().then((size) {
        if (mounted) setState(() => _sizes[path] = size);
      }, onError: (_) {});
    }
  }

  /// Whether closing would lose something the person wrote.
  bool get _hasDraft => _attachments.isNotEmpty || [..._fields.values, _subject, _body].any((c) => c.text.trim().isNotEmpty);

  void _close() => Navigator.of(context).pop();

  /// Closes at once when nothing would be lost; otherwise asks in the bar at the bottom.
  void _requestClose() {
    if (_sending) return;
    if (!_hasDraft) return _close();
    setState(() => _confirmingDiscard = true);
  }

  /// Esc takes back the discard question first, then closes.
  void _escape() {
    if (_confirmingDiscard) return setState(() => _confirmingDiscard = false);
    _requestClose();
  }

  void _fail(String message) => setState(() => _error = message);

  Future<void> _send() async {
    if (_sending) return;
    _confirmingDiscard = false;
    final from = _from;
    if (from == null) return _fail('Add an account in Mailstream to send from.');
    _invalid.clear();
    final lists = <_Field, List<String>>{};
    for (final field in _Field.values) {
      final parsed = _parseRecipients(_fields[field]!.text);
      if (parsed.invalid case final invalid?) {
        _invalid.add(field);
        if (field == _Field.bcc) _showBcc = true;
        _focus[field]!.requestFocus();
        return _fail('${field.label}: “$invalid” is not an email address.');
      }
      lists[field] = parsed.recipients;
    }
    if (lists[_Field.to]!.isEmpty) {
      _invalid.add(_Field.to);
      _focus[_Field.to]!.requestFocus();
      return _fail('Add at least one recipient.');
    }
    setState(() {
      _sending = true;
      _error = null;
    });
    try {
      await widget.state.send(engine.Draft(
        from: from,
        to: lists[_Field.to]!,
        cc: lists[_Field.cc]!,
        bcc: lists[_Field.bcc]!,
        subject: _subject.text.trim(),
        body: _body.text,
        attachments: List.of(_attachments),
      ));
      if (mounted) _close();
    } on engine.MailException catch (error) {
      if (!mounted) return;
      setState(() {
        _sending = false;
        _error = describe(error);
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return PopScope(
      // Pops other than Send and the discard bar (Esc from the route) go through the same question.
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _escape();
      },
      child: CallbackShortcuts(
        bindings: {
          const SingleActivator(LogicalKeyboardKey.enter, meta: true): _send,
          const SingleActivator(LogicalKeyboardKey.keyW, meta: true): _requestClose,
          const SingleActivator(LogicalKeyboardKey.keyV, meta: true): _paste,
          const SingleActivator(LogicalKeyboardKey.escape): _escape,
        },
        child: Sheet(
          width: 680,
          height: 620,
          child: _droppable(context, Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _titleBar(context),
                _from == null ? const SizedBox.shrink() : _row(context, label: 'From', child: _fromPicker(context)),
                _recipients(context, _Field.to),
                _recipients(
                  context,
                  _Field.cc,
                  trailing: _showBcc
                      ? null
                      : PushButton(
                          controlSize: ControlSize.regular,
                          secondary: true,
                          onPressed: _sending
                              ? null
                              : () {
                                  setState(() => _showBcc = true);
                                  _focus[_Field.bcc]!.requestFocus();
                                },
                          child: const Text('Bcc'),
                        ),
                ),
                if (_showBcc) _recipients(context, _Field.bcc),
                _row(
                  context,
                  child: _bareField(
                    controller: _subject,
                    enabled: !_sending,
                    style: text.title3,
                    placeholder: 'Subject',
                  ),
                ),
                Expanded(
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(20, 12, 20, 12),
                    child: _bareField(
                      controller: _body,
                      enabled: !_sending,
                      expands: true,
                      style: text.body.copyWith(height: 1.55),
                    ),
                  ),
                ),
                if (_attachments.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.fromLTRB(20, 0, 20, 12),
                    child: Wrap(spacing: 8, runSpacing: 8, children: [for (final path in _attachments) _attachmentChip(context, path)]),
                  ),
                if (_sending || _error != null)
                  Container(
                    padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 10),
                    decoration: BoxDecoration(border: Border(top: BorderSide(color: colors.separator, width: 0.5))),
                    child: Text(
                      _sending ? 'Sending…' : _error!,
                      style: text.subheadline.copyWith(color: _sending ? colors.secondaryLabel : colors.red),
                    ),
                  ),
                if (_confirmingDiscard) _discardBar(context),
              ],
          )),
        ),
      ),
    );
  }

  /// The window's content, under the drop target while files are dragged over it.
  Widget _droppable(BuildContext context, Widget child) => Stack(children: [
    child,
    if (_dropping) Positioned.fill(child: IgnorePointer(child: _dropTarget(context))),
  ]);

  Widget _dropTarget(BuildContext context) {
    final colors = context.colors;
    return Container(
      margin: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        color: colors.accentFill.withValues(alpha: 0.92),
        borderRadius: BorderRadius.circular(Panes.sheetCorner - 8),
        border: Border.all(color: colors.accent, width: 2),
      ),
      child: Column(mainAxisAlignment: MainAxisAlignment.center, children: [
        MacosIcon(CupertinoIcons.paperclip, size: 36, color: colors.onAccentFill),
        const SizedBox(height: 12),
        Text('Drop files to attach', style: context.text.title3.copyWith(color: colors.onAccentFill)),
      ]),
    );
  }

  /// A file's name and size, and a button removing it.
  Widget _attachmentChip(BuildContext context, String path) {
    final size = _sizes[path];
    final name = path.substring(path.lastIndexOf('/') + 1);
    return MacosTooltip(
      message: path,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 300),
        child: Pill(
          label: size == null ? name : '$name  ·  ${fileSize(size)}',
          leading: MacosIcon(CupertinoIcons.paperclip, size: 13, color: context.colors.secondaryLabel),
          onDelete: _sending
              ? null
              : () => setState(() {
                  _attachments.remove(path);
                  _sizes.remove(path);
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
  }) => MacosTextField(
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
    placeholderStyle: style.copyWith(color: context.colors.border),
    decoration: null,
    focusedDecoration: null,
    padding: EdgeInsets.zero,
    onChanged: onChanged,
  );

  Widget _titleBar(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(12, 10, 16, 6),
    child: Row(children: [
      GlassCapsule(
        child: ToolButton(tooltip: 'Close (Esc)', icon: CupertinoIcons.xmark, onPressed: _sending ? null : _requestClose),
      ),
      const SizedBox(width: 10),
      Expanded(child: Text('New Message', style: context.text.title3.copyWith(fontWeight: FontWeight.w700))),
      GlassCapsule(
        child: ToolButton(
          tooltip: 'Attach files (or drop them on the message)',
          icon: CupertinoIcons.paperclip,
          onPressed: _sending ? null : _pick,
        ),
      ),
      const SizedBox(width: 10),
      MacosTooltip(
        message: '⌘ Return',
        child: PushButton(
          controlSize: ControlSize.large,
          onPressed: _sending ? null : _send,
          child: Row(mainAxisSize: MainAxisSize.min, children: [
            _sending ? const Spinner(radius: 6) : const MacosIcon(CupertinoIcons.paperplane_fill, size: 14, color: MacosColors.white),
            const SizedBox(width: 6),
            Text(_sending ? 'Sending' : 'Send'),
          ]),
        ),
      ),
    ]),
  );

  /// One header line over a hairline: a right-aligned label, then the field.
  Widget _row(BuildContext context, {String? label, bool invalid = false, required Widget child, Widget? trailing}) {
    final colors = context.colors;
    return Container(
      height: 44,
      margin: const EdgeInsets.only(left: 20),
      padding: const EdgeInsets.only(right: 16),
      decoration: BoxDecoration(border: Border(bottom: BorderSide(color: colors.separator, width: 0.5))),
      child: Row(children: [
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
      ]),
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
        enabled: !_sending,
        autofocus: field == _Field.to,
        style: context.text.body,
        onChanged: (_) {
          if (_invalid.contains(field)) setState(() => _invalid.remove(field));
        },
      ),
    ),
  );

  /// The people for the address being typed in a field, without those already in To, Cc or Bcc.
  Future<List<Suggestion>> _suggest(String text) async {
    final typed = _splitRecipients(text).last.trim();
    if (typed.isEmpty || _sending) return const [];
    final people = await widget.state.searchContacts(typed);
    final chosen = {
      for (final field in _Field.values)
        for (final part in _splitRecipients(_fields[field]!.text))
          if (part.trim().isNotEmpty && part.trim() != typed) Address.parse(part).email.toLowerCase(),
    };
    return [for (final person in people) if (!chosen.contains(person.email.toLowerCase())) Suggestion(person)];
  }

  /// Puts [person] in place of the address being typed in [field], ready for the next one.
  void _complete(_Field field, Address person) {
    final controller = _fields[field]!;
    final typing = _splitRecipients(controller.text).last;
    final before = controller.text.substring(0, controller.text.length - typing.length);
    final text = '${before.isEmpty || before.endsWith(' ') ? before : '$before '}${_recipient(person)}, ';
    controller.value = TextEditingValue(text: text, selection: TextSelection.collapsed(offset: text.length));
    if (_invalid.contains(field)) setState(() => _invalid.remove(field));
  }

  Widget _fromPicker(BuildContext context) {
    final accounts = widget.state.accounts;
    String title(String id) {
      final account = widget.state.account(id);
      return account == null || account.title == id ? id : '${account.title} <$id>';
    }

    if (accounts.length < 2) return Text(title(_from!), style: context.text.body);
    return Align(
      alignment: Alignment.centerLeft,
      child: MacosPopupButton<String>(
        value: _from,
        onChanged: _sending ? null : (id) => setState(() => _from = id),
        items: [for (final account in accounts) MacosPopupMenuItem(value: account.id, child: Text(title(account.id)))],
      ),
    );
  }

  Widget _discardBar(BuildContext context) {
    final colors = context.colors;
    return Container(
      color: colors.fill,
      padding: const EdgeInsets.fromLTRB(20, 8, 12, 8),
      child: Row(children: [
        Expanded(child: Text('Discard this message?', style: context.text.body)),
        PushButton(
          controlSize: ControlSize.regular,
          secondary: true,
          onPressed: () => setState(() => _confirmingDiscard = false),
          child: const Text('Keep Editing'),
        ),
        const SizedBox(width: 8),
        PushButton(controlSize: ControlSize.regular, color: colors.red, onPressed: _close, child: const Text('Discard')),
      ]),
    );
  }
}
