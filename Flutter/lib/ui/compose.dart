import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'package:mailstream/mailstream.dart' as engine;

import '../state/mail_state.dart';
import 'design.dart';
import 'format.dart';

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
  return showDialog<void>(
    context: context,
    barrierDismissible: false,
    builder: (_) => ComposeWindow(state: state, from: from),
  );
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
        child: Dialog(
          insetPadding: const EdgeInsets.all(32),
          clipBehavior: Clip.antiAlias,
          backgroundColor: colors.surface,
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Panes.corner)),
          child: _droppable(context, SizedBox(
            width: 680,
            height: 620,
            child: Column(
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
                      : TextButton(
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
                  child: TextField(
                    controller: _subject,
                    enabled: !_sending,
                    style: text.titleMedium,
                    decoration: const InputDecoration.collapsed(hintText: 'Subject'),
                  ),
                ),
                Expanded(
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(20, 12, 20, 12),
                    child: TextField(
                      controller: _body,
                      enabled: !_sending,
                      expands: true,
                      maxLines: null,
                      textAlignVertical: TextAlignVertical.top,
                      keyboardType: TextInputType.multiline,
                      style: text.bodyLarge?.copyWith(height: 1.55),
                      decoration: const InputDecoration.collapsed(hintText: ''),
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
                    decoration: BoxDecoration(border: Border(top: BorderSide(color: colors.outlineVariant))),
                    child: Text(
                      _sending ? 'Sending…' : _error!,
                      style: text.bodySmall?.copyWith(color: _sending ? colors.onSurfaceVariant : colors.error),
                    ),
                  ),
                if (_confirmingDiscard) _discardBar(context),
              ],
            ),
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
        color: colors.primaryContainer.withValues(alpha: 0.92),
        borderRadius: BorderRadius.circular(Panes.corner - 4),
        border: Border.all(color: colors.primary, width: 2),
      ),
      child: Column(mainAxisAlignment: MainAxisAlignment.center, children: [
        Icon(Icons.attach_file, size: 40, color: colors.onPrimaryContainer),
        const SizedBox(height: 12),
        Text('Drop files to attach', style: context.text.titleMedium?.copyWith(color: colors.onPrimaryContainer)),
      ]),
    );
  }

  /// A file's name and size, and a button removing it.
  Widget _attachmentChip(BuildContext context, String path) {
    final size = _sizes[path];
    final name = path.substring(path.lastIndexOf('/') + 1);
    return InputChip(
      avatar: const Icon(Icons.attach_file, size: 18),
      label: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 260),
        child: Text(size == null ? name : '$name  ·  ${fileSize(size)}', maxLines: 1, overflow: TextOverflow.ellipsis),
      ),
      tooltip: path,
      deleteButtonTooltipMessage: 'Remove',
      onDeleted: _sending
          ? null
          : () => setState(() {
              _attachments.remove(path);
              _sizes.remove(path);
            }),
    );
  }

  Widget _titleBar(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(8, 10, 16, 6),
    child: Row(children: [
      IconButton(tooltip: 'Close (Esc)', icon: const Icon(Icons.close), onPressed: _sending ? null : _requestClose),
      const SizedBox(width: 4),
      Expanded(child: Text('New Message', style: context.text.titleLarge)),
      IconButton(
        tooltip: 'Attach files (or drop them on the message)',
        icon: const Icon(Icons.attach_file),
        onPressed: _sending ? null : _pick,
      ),
      const SizedBox(width: 8),
      Tooltip(
        message: '⌘ Return',
        child: FilledButton.icon(
          onPressed: _sending ? null : _send,
          icon: _sending
              ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
              : const Icon(Icons.send),
          label: Text(_sending ? 'Sending' : 'Send'),
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
      decoration: BoxDecoration(border: Border(bottom: BorderSide(color: colors.outlineVariant))),
      child: Row(children: [
        if (label != null) ...[
          SizedBox(
            width: 48,
            child: Text(
              '$label:',
              textAlign: TextAlign.end,
              style: context.text.bodyMedium?.copyWith(color: invalid ? colors.error : colors.onSurfaceVariant),
            ),
          ),
          const SizedBox(width: 10),
        ],
        Expanded(child: child),
        ?trailing,
      ]),
    );
  }

  Widget _recipients(BuildContext context, _Field field, {Widget? trailing}) => _row(
    context,
    label: field.label,
    invalid: _invalid.contains(field),
    trailing: trailing,
    child: TextField(
      controller: _fields[field],
      focusNode: _focus[field],
      enabled: !_sending,
      autofocus: field == _Field.to,
      style: context.text.bodyLarge,
      decoration: const InputDecoration.collapsed(hintText: ''),
      onChanged: (_) {
        if (_invalid.contains(field)) setState(() => _invalid.remove(field));
      },
    ),
  );

  Widget _fromPicker(BuildContext context) {
    final accounts = widget.state.accounts;
    String title(String id) {
      final account = widget.state.account(id);
      return account == null || account.title == id ? id : '${account.title} <$id>';
    }

    if (accounts.length < 2) return Text(title(_from!), style: context.text.bodyLarge);
    return DropdownButtonHideUnderline(
      child: DropdownButton<String>(
        value: _from,
        isExpanded: true,
        focusColor: Colors.transparent,
        style: context.text.bodyLarge,
        borderRadius: BorderRadius.circular(12),
        onChanged: _sending ? null : (id) => setState(() => _from = id),
        items: [for (final account in accounts) DropdownMenuItem(value: account.id, child: Text(title(account.id)))],
      ),
    );
  }

  Widget _discardBar(BuildContext context) {
    final colors = context.colors;
    return Container(
      color: colors.surfaceContainerHigh,
      padding: const EdgeInsets.fromLTRB(20, 8, 12, 8),
      child: Row(children: [
        Expanded(child: Text('Discard this message?', style: context.text.bodyMedium)),
        TextButton(onPressed: () => setState(() => _confirmingDiscard = false), child: const Text('Keep Editing')),
        const SizedBox(width: 8),
        FilledButton.tonal(
          style: FilledButton.styleFrom(backgroundColor: colors.errorContainer, foregroundColor: colors.onErrorContainer),
          onPressed: _close,
          child: const Text('Discard'),
        ),
      ]),
    );
  }
}
