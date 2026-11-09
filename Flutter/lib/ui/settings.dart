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

/// Opens Settings (⌘,): the accounts, as the app's Accounts window has them (add through the
/// browser, rename, remove), and every setting of settings.json. The file is the one
/// Mailstream.app and `mailstream config` use, so a change here is theirs too, and edits made
/// there show here as the engine reports them.
Future<void> openSettings(BuildContext context, MailState state) => showSheet<void>(context, (_) => SettingsWindow(state: state));

/// Sections of settings.json by key prefix, in the order they show.
const _sections = [
  ('Sync', ['sync.', 'chat.']),
  ('AI tagging', ['aiTagging.']),
  ('Auto-read', ['autoRead.']),
  ('Remote content', ['remoteContent.']),
  ('Performance', ['flutter.showFrameRate']),
];

/// Short names for the keys; the engine's description sits under each.
const _titles = {
  'aiTagging.enabled': 'Sort and flag mail with AI',
  'aiTagging.backend': 'Tagged by',
  'aiTagging.model': 'Model',
  'aiTagging.ompPath': 'omp executable',
  'aiTagging.batchSize': 'Batch size',
  'aiTagging.passLimit': 'Conversations per pass',
  'aiTagging.backoffSeconds': 'Wait after a failed run',
  'aiTagging.syncAccounts': 'Sync tags with the server',
  'autoRead.enabled': 'Mark stale notifications and newsletters read',
  'autoRead.days': 'Days before marking read',
  'remoteContent.trustedSenders': 'Trusted senders',
  'sync.pollSeconds': 'Mail polling',
  'sync.pushPollSeconds': 'Polling while push is connected',
  'chat.pollSeconds': 'Google Chat polling',
  'flutter.showFrameRate': 'Show frame rate',
};

/// Names for the words a choice setting takes.
const _choiceTitles = {
  'omp': 'omp',
  'apple': 'Apple Intelligence',
};

class SettingsWindow extends StatelessWidget {
  const SettingsWindow({super.key, required this.state});

  final MailState state;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return Sheet(
      width: 720,
      height: 680,
      child: ListenableBuilder(
        listenable: state,
        builder: (context, _) {
          final settings = state.settings;
          return Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(24, 16, 16, 8),
                child: Row(
                  children: [
                    Expanded(child: Text('Settings', style: text.title3.copyWith(fontWeight: FontWeight.w700))),
                    GlassCapsule(
                      child: ToolButton(tooltip: 'Close (Esc)', icon: CupertinoIcons.xmark, onPressed: () => Navigator.of(context).pop()),
                    ),
                  ],
                ),
              ),
              Expanded(
                child: ListView(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                  children: [
                    _Heading('Accounts'),
                    _Accounts(state: state),
                    if (settings == null)
                      const Padding(
                        padding: EdgeInsets.all(24),
                        child: Center(child: Spinner()),
                      )
                    else ...[
                      for (final issue in settings.issues) _Issue(issue),
                      for (final (title, prefixes) in _sections) ...[
                        _Heading(title),
                        // Why tagging stopped, as the engine reports it: omp or Apple
                        // Intelligence unavailable, a failed run.
                        if (prefixes.contains('aiTagging.') && state.taggingProblem != null) _Issue(state.taggingProblem!),
                        _Group(
                          children: [
                            for (final entry in settings.settings)
                              if (prefixes.any(entry.key.startsWith))
                                entry.key == _syncAccounts
                                    ? _SyncAccountsRow(state: state, entry: entry)
                                    : _SettingRow(state: state, entry: entry),
                          ],
                        ),
                        if (title == 'Auto-read') _AutoReadLog(state: state),
                      ],
                      _Group(
                        children: [
                          for (final entry in settings.settings)
                            if (!_sections.any((s) => s.$2.any(entry.key.startsWith)))
                              _SettingRow(state: state, entry: entry),
                        ],
                      ),
                      Padding(
                        padding: const EdgeInsets.fromLTRB(8, 12, 8, 0),
                        child: Selectable(
                          child: Text(
                            'Saved in ${settings.file}, shared with Mailstream and `mailstream config`.',
                            style: text.subheadline.copyWith(color: colors.secondaryLabel),
                          ),
                        ),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}

class _Heading extends StatelessWidget {
  const _Heading(this.title);

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(8, 16, 8, 6),
    child: Text(title, style: context.text.callout.copyWith(color: context.colors.secondaryLabel, fontWeight: FontWeight.w600)),
  );
}

/// A rounded card of rows divided by hairlines, as System Settings groups them; nothing when it
/// has none.
class _Group extends StatelessWidget {
  const _Group({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    if (children.isEmpty) return const SizedBox.shrink();
    final colors = context.colors;
    return Container(
      clipBehavior: Clip.antiAlias,
      decoration: ShapeDecoration(
        color: colors.fill,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(Panes.corner),
          side: BorderSide(color: colors.separator, width: 0.5),
        ),
      ),
      child: Column(
        children: [
          for (final (index, child) in children.indexed) ...[
            if (index > 0) const Hairline(indent: 16, endIndent: 16),
            child,
          ],
        ],
      ),
    );
  }
}

/// A row of a [_Group], as System Settings lays them out: a glyph or mark, the title over a line
/// of small print, the control at the trailing end.
class _Row extends StatelessWidget {
  const _Row({this.leading, required this.title, this.subtitle, this.trailing, this.onTap});

  final Widget? leading;
  final Widget title;
  final Widget? subtitle;
  final Widget? trailing;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final row = Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 9),
      child: Row(children: [
        if (leading != null) ...[leading!, const SizedBox(width: 12)],
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              DefaultTextStyle.merge(style: context.text.body.copyWith(color: colors.label), child: title),
              if (subtitle != null)
                Padding(
                  padding: const EdgeInsets.only(top: 2),
                  child: DefaultTextStyle.merge(style: context.text.subheadline.copyWith(color: colors.secondaryLabel), child: subtitle!),
                ),
            ],
          ),
        ),
        if (trailing != null) ...[const SizedBox(width: 12), trailing!],
      ]),
    );
    if (onTap == null) return row;
    return Hoverable(onTap: onTap, radius: 0, child: row);
  }
}

class _Issue extends StatelessWidget {
  const _Issue(this.message);

  final String message;

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
        decoration: BoxDecoration(color: colors.redFill, borderRadius: BorderRadius.circular(Panes.corner)),
        child: Row(children: [
          MacosIcon(CupertinoIcons.exclamationmark_circle, size: 17, color: colors.onRedFill),
          const SizedBox(width: 10),
          Expanded(child: Text(message, style: context.text.body.copyWith(color: colors.onRedFill))),
        ]),
      ),
    );
  }
}

// MARK: - Tag sync

const _syncAccounts = 'aiTagging.syncAccounts';

/// `aiTagging.syncAccounts` as a switch per mail account: on, that account's AI tags sync both
/// ways with its Gmail labels or Outlook categories. Accounts listed but no longer signed in show
/// too, so they can be switched off.
class _SyncAccountsRow extends StatefulWidget {
  const _SyncAccountsRow({required this.state, required this.entry});

  final MailState state;
  final engine.SettingEntry entry;

  @override
  State<_SyncAccountsRow> createState() => _SyncAccountsRowState();
}

class _SyncAccountsRowState extends State<_SyncAccountsRow> {
  String? _error;

  List<String> get _listed => switch (widget.entry.value) {
    engine.AddressesSettingValue(:final addresses) => addresses,
    _ => const [],
  };

  Future<void> _set(String address, bool on) async {
    final listed = _listed;
    final next = on ? [...listed, address] : [for (final a in listed) if (a != address) a];
    try {
      await widget.state.setSetting(_syncAccounts, engine.AddressesSettingValue(next));
      if (mounted) setState(() => _error = null);
    } on engine.MailException catch (error) {
      if (mounted) setState(() => _error = describe(error));
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final listed = _listed;
    final accounts = [
      for (final account in widget.state.accounts)
        if (account.services.contains('mail')) (address: account.id.toLowerCase(), title: account.title, provider: account.provider),
    ];
    final rows = [
      ...accounts,
      for (final address in listed)
        if (!accounts.any((a) => a.address == address)) (address: address, title: address, provider: ''),
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 10, 8, 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(_titles[_syncAccounts]!, style: text.body),
          const SizedBox(height: 2),
          Text(widget.entry.summary, style: text.subheadline.copyWith(color: colors.secondaryLabel)),
          Text(_syncAccounts, style: text.subheadline.copyWith(color: colors.border)),
          if (rows.isEmpty)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Text('Sign in to a Gmail or Outlook account to sync its tags.', style: text.subheadline.copyWith(color: colors.secondaryLabel)),
            ),
          for (final row in rows)
            Padding(
              padding: const EdgeInsets.only(top: 8, right: 32),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      [
                        row.title,
                        if (row.provider.isNotEmpty) row.provider == 'google' ? 'Gmail labels' : 'Outlook categories',
                        if (row.provider.isEmpty) 'not signed in',
                      ].join('  ·  '),
                      style: text.body,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                  MacosSwitch(value: listed.contains(row.address), onChanged: (on) => _set(row.address, on)),
                ],
              ),
            ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(_error!, style: text.subheadline.copyWith(color: colors.red)),
            ),
        ],
      ),
    );
  }
}

// MARK: - Auto-read

/// What auto-read marked read and nobody undid, newest first, each with Mark Unread, and Mark
/// All Unread above them; a line saying nothing was marked while it is on and has done nothing.
class _AutoReadLog extends StatelessWidget {
  const _AutoReadLog({required this.state});

  /// Rows shown; the rest stay in the engine's log (`mailstream mail autoread`).
  static const limit = 50;

  final MailState state;

  @override
  Widget build(BuildContext context) {
    final log = state.autoRead;
    engine.SettingValue? setting(String key) => state.settings?.settings.where((e) => e.key == key).firstOrNull?.value;
    final enabled = switch (setting('autoRead.enabled')) {
      engine.FlagSettingValue(:final enabled) => enabled,
      _ => false,
    };
    final days = switch (setting('autoRead.days')) {
      engine.NumberSettingValue(:final value) => '$value',
      _ => '${log?.days ?? 3}',
    };
    if (log == null || (log.entries.isEmpty && !enabled)) return const SizedBox.shrink();
    final colors = context.colors;
    final text = context.text;
    final entries = log.entries;
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: _Group(
        children: [
          _Row(
            title: const Text('Recently marked read'),
            subtitle: Text(
              entries.isEmpty
                  ? 'Nothing yet. Unread notifications and newsletters older than $days days show here once marked read.'
                  : '${entries.length} conversation${entries.length == 1 ? '' : 's'}. Marking one unread keeps auto-read away from it until new mail arrives.',
            ),
            trailing: entries.isEmpty
                ? null
                : PushButton(
                    controlSize: ControlSize.regular,
                    secondary: true,
                    onPressed: () => state.undoAutoRead([for (final e in entries) e.id]),
                    child: const Text('Mark All Unread'),
                  ),
          ),
          for (final entry in entries.take(limit))
            _Row(
              title: Text.rich(
                TextSpan(children: [
                  TextSpan(text: entry.sender.isEmpty ? '(Unknown sender)' : entry.sender, style: const TextStyle(fontWeight: FontWeight.w600)),
                  TextSpan(text: '  ${entry.subject}'),
                ]),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              subtitle: Text(
                [
                  entry.category == 'newsletter' ? 'Newsletter' : 'Notification',
                  entry.reason == 'superseded' ? 'a newer one from the same sender arrived' : 'unread too long',
                  ?_markedAt(entry.at),
                ].join('  ·  '),
                style: text.subheadline.copyWith(color: colors.secondaryLabel),
              ),
              trailing: ToolButton(
                tooltip: 'Mark Unread',
                icon: CupertinoIcons.envelope_badge,
                onPressed: () => state.undoAutoRead([entry.id]),
              ),
            ),
        ],
      ),
    );
  }

  static String? _markedAt(String iso) {
    final date = DateTime.tryParse(iso)?.toLocal();
    return date == null ? null : 'marked ${listDate(date, DateTime.now())}';
  }
}

// MARK: - Accounts

class _Accounts extends StatefulWidget {
  const _Accounts({required this.state});

  final MailState state;

  @override
  State<_Accounts> createState() => _AccountsState();
}

class _AccountsState extends State<_Accounts> {
  /// The provider being signed in to, while the browser is open.
  String? _signingIn;
  ({String text, bool error})? _status;

  MailState get state => widget.state;

  static String _providerName(String provider) => provider == 'google' ? 'Google' : 'Microsoft';

  Future<void> _add(String provider) async {
    final suggested = await state.suggestedClient(provider);
    if (!mounted) return;
    final client = await showSheet<({String id, String? secret})>(
      context,
      (_) => _ClientDialog(provider: provider, suggested: suggested),
    );
    if (client == null || !mounted) return;
    setState(() {
      _signingIn = provider;
      _status = (text: 'Waiting for the browser…', error: false);
    });
    try {
      final account = await state.addAccount(provider, clientId: client.id, clientSecret: client.secret);
      _status = (text: 'Signed in as ${account.id}.', error: false);
    } on engine.MailException catch (error) {
      _status = (text: describe(error), error: true);
    }
    if (mounted) setState(() => _signingIn = null);
  }

  Future<void> _rename(Account account) async {
    final label = await showSheet<String>(context, (context) => _RenameDialog(account: account));
    if (label != null && label != account.label) await state.renameAccount(account.id, label);
  }

  Future<void> _remove(Account account) async {
    final confirmed = await showMacosAlertDialog<bool>(
      context: context,
      builder: (context) => MacosAlertDialog(
        appIcon: const MacosIcon(CupertinoIcons.person_crop_circle_badge_minus, size: 56),
        title: Text('Remove ${account.title}?', style: context.text.headline),
        message: Text(
          'Mailstream signs out of ${account.id} and deletes its saved sign-in. '
          'Mail on the server is not affected; you can add the account again later.',
          textAlign: TextAlign.center,
          style: context.text.subheadline,
        ),
        horizontalActions: true,
        secondaryButton: PushButton(
          controlSize: ControlSize.large,
          secondary: true,
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('Cancel'),
        ),
        primaryButton: PushButton(
          controlSize: ControlSize.large,
          color: context.colors.red,
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('Remove'),
        ),
      ),
    );
    if (confirmed == true) {
      setState(() => _status = null);
      await state.removeAccount(account.id);
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final status = _status;
    return _Group(
      children: [
        if (state.accounts.isEmpty)
          _Row(
            leading: MacosIcon(CupertinoIcons.person_badge_plus, size: 20, color: colors.secondaryLabel),
            title: const Text('No accounts'),
            subtitle: const Text('Sign in to bring in mail, contacts and Google Chat.'),
          ),
        for (final account in state.accounts)
          _Row(
            leading: IconAvatar(
              size: 32,
              icon: account.provider == 'google' ? CupertinoIcons.envelope_fill : CupertinoIcons.mail_solid,
              background: accountPalette[state.accountIndex(account.id) % accountPalette.length],
              foreground: MacosColors.white,
            ),
            title: Text(account.title),
            subtitle: Text(
              [
                if (account.label.isNotEmpty && account.label != account.id) account.id,
                _providerName(account.provider),
                if (account.services.isNotEmpty)
                  account.services
                      .map(
                        (s) => switch (s) {
                          'mail' => 'Mail',
                          'chat' => 'Chat',
                          'calendar' => 'Calendar',
                          _ => 'Contacts',
                        },
                      )
                      .join(', '),
              ].join(' · '),
            ),
            onTap: () => _rename(account),
            trailing: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                ToolButton(tooltip: 'Rename', icon: CupertinoIcons.pencil, onPressed: () => _rename(account)),
                ToolButton(
                  tooltip: 'Remove',
                  icon: CupertinoIcons.minus_circle,
                  onPressed: _signingIn == null ? () => _remove(account) : null,
                ),
              ],
            ),
          ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
          child: Row(
            children: [
              for (final provider in ['google', 'microsoft']) ...[
                PushButton(
                  controlSize: ControlSize.regular,
                  secondary: true,
                  onPressed: _signingIn == null ? () => _add(provider) : null,
                  child: Text('${_providerName(provider)} Account…'),
                ),
                const SizedBox(width: 8),
              ],
              if (_signingIn != null)
                const Spinner(radius: 7),
              if (status != null) ...[
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    status.text,
                    style: text.subheadline.copyWith(color: status.error ? colors.red : colors.secondaryLabel),
                  ),
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }
}

/// The OAuth client to sign in with, filled in with the one used last (or the build's). Its
/// secret never leaves the engine: an empty secret field with that client's id keeps the saved
/// one.
class _ClientDialog extends StatefulWidget {
  const _ClientDialog({required this.provider, required this.suggested});

  final String provider;
  final engine.OAuthClientInfo? suggested;

  @override
  State<_ClientDialog> createState() => _ClientDialogState();
}

class _ClientDialogState extends State<_ClientDialog> {
  late final _id = TextEditingController(text: widget.suggested?.id ?? '');
  final _secret = TextEditingController();
  late final _focusSecret = _id.text.isNotEmpty;
  String? _error;

  bool get _google => widget.provider == 'google';

  /// The id in the field is the suggested client's, whose secret the engine keeps.
  bool get _savedSecret {
    final suggested = widget.suggested;
    return suggested != null && suggested.hasSecret && _id.text.trim() == suggested.id;
  }

  @override
  void initState() {
    super.initState();
    _id.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _id.dispose();
    _secret.dispose();
    super.dispose();
  }

  void _submit() {
    final id = _id.text.trim(), secret = _secret.text.trim();
    if (id.isEmpty) {
      setState(() => _error = 'Enter your OAuth client\'s ID to sign in.');
      return;
    }
    if (_google && secret.isEmpty && !_savedSecret) {
      setState(() => _error = 'Google needs the client secret too; the Cloud Console shows it next to the client ID.');
      return;
    }
    Navigator.of(context).pop((id: id, secret: secret.isEmpty ? null : secret));
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return Sheet(
      width: 460,
      title: _google ? 'Sign in with Google' : 'Sign in with Microsoft',
      actions: [
        PushButton(
          controlSize: ControlSize.large,
          secondary: true,
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        PushButton(controlSize: ControlSize.large, onPressed: _submit, child: const Text('Continue in Browser')),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              _google
                  ? 'The client ID and secret of your Google Cloud OAuth client (type Desktop app). '
                        'Mailstream then opens Google\'s sign-in in your browser.'
                  : 'The application ID of your Microsoft Entra app, and its secret unless it is a public client. '
                        'Mailstream then opens Microsoft\'s sign-in in your browser.',
              style: text.body.copyWith(color: colors.label),
            ),
            if (widget.suggested != null) ...[
              const SizedBox(height: 4),
              Text(
                'Filled in with the client you signed in with last; change it to use another.',
                style: text.subheadline.copyWith(color: colors.secondaryLabel),
              ),
            ],
            const SizedBox(height: 14),
            _field(
              _google ? 'Client ID' : 'Application ID',
              MacosTextField(
                controller: _id,
                autofocus: !_focusSecret,
                style: text.body,
                placeholder: _google ? 'Client ID' : 'Application (client) ID',
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 5),
                onSubmitted: (_) => _submit(),
              ),
            ),
            const SizedBox(height: 8),
            _field(
              'Client secret',
              MacosTextField(
                controller: _secret,
                autofocus: _focusSecret,
                obscureText: true,
                style: text.body,
                placeholder: _savedSecret
                    ? 'Saved; leave empty to keep it'
                    : _google
                    ? 'Client secret'
                    : 'Empty for a public client',
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 5),
                onSubmitted: (_) => _submit(),
              ),
            ),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(_error!, style: text.subheadline.copyWith(color: colors.red)),
              ),
          ],
        ),
      ),
    );
  }

  Widget _field(String label, Widget field) => Row(
    children: [
      SizedBox(
        width: 104,
        child: Text('$label:', textAlign: TextAlign.end, style: context.text.body.copyWith(color: context.colors.label)),
      ),
      const SizedBox(width: 8),
      Expanded(child: field),
    ],
  );
}

class _RenameDialog extends StatefulWidget {
  const _RenameDialog({required this.account});

  final Account account;

  @override
  State<_RenameDialog> createState() => _RenameDialogState();
}

class _RenameDialogState extends State<_RenameDialog> {
  late final _label = TextEditingController(text: widget.account.label);

  @override
  void dispose() {
    _label.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    return Sheet(
      width: 380,
      title: 'Account label',
      actions: [
        PushButton(
          controlSize: ControlSize.large,
          secondary: true,
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        PushButton(
          controlSize: ControlSize.large,
          onPressed: () => Navigator.of(context).pop(_label.text.trim()),
          child: const Text('Save'),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            MacosTextField(
              controller: _label,
              autofocus: true,
              style: text.body,
              placeholder: widget.account.id,
              padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 5),
              onSubmitted: (value) => Navigator.of(context).pop(value.trim()),
            ),
            const SizedBox(height: 6),
            Text('Shown in the sidebar; empty shows the address.', style: text.subheadline.copyWith(color: colors.secondaryLabel)),
          ],
        ),
      ),
    );
  }
}

// MARK: - Settings

/// One setting: its name and the engine's description, the control its type takes, and a reset
/// button while it differs from the default. The engine checks every value; its refusal shows
/// under the row.
class _SettingRow extends StatefulWidget {
  const _SettingRow({required this.state, required this.entry});

  final MailState state;
  final engine.SettingEntry entry;

  @override
  State<_SettingRow> createState() => _SettingRowState();
}

class _SettingRowState extends State<_SettingRow> {
  final _field = TextEditingController();
  final _focus = FocusNode();
  String? _error;

  engine.SettingEntry get entry => widget.entry;

  @override
  void initState() {
    super.initState();
    _field.text = _fieldText(entry.value);
    _focus.addListener(() {
      if (!_focus.hasFocus) _commitField();
    });
  }

  @override
  void didUpdateWidget(_SettingRow old) {
    super.didUpdateWidget(old);
    // Values written elsewhere (Mailstream.app, the CLI, an editor) replace the field's text
    // unless it is being edited.
    if (old.entry.value != entry.value && !_focus.hasFocus) _field.text = _fieldText(entry.value);
  }

  @override
  void dispose() {
    _field.dispose();
    _focus.dispose();
    super.dispose();
  }

  static String _fieldText(engine.SettingValue value) => switch (value) {
    engine.NumberSettingValue(:final value) => '$value',
    engine.TextSettingValue(:final text) => text,
    _ => '',
  };

  Future<void> _write(engine.SettingValue? value) async {
    try {
      await widget.state.setSetting(entry.key, value);
      if (mounted) setState(() => _error = null);
    } on engine.MailException catch (error) {
      if (mounted) setState(() => _error = describe(error));
    }
  }

  void _commitField() {
    final text = _field.text.trim();
    switch (entry.value) {
      case engine.NumberSettingValue(:final value):
        final number = int.tryParse(text);
        if (number == null) {
          setState(() => _error = 'Enter a whole number.');
        } else if (number != value) {
          _write(engine.NumberSettingValue(number));
        }
      case engine.TextSettingValue(text: final current):
        if (text != current) _write(engine.TextSettingValue(text));
      default:
        break;
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final text = context.text;
    final range = entry.minimum != null && entry.maximum != null ? '${entry.minimum}–${entry.maximum}' : null;
    final control = switch (entry.value) {
      engine.FlagSettingValue(:final enabled) => MacosSwitch(
        value: enabled,
        onChanged: (on) => _write(engine.FlagSettingValue(on)),
      ),
      engine.NumberSettingValue() => SizedBox(
        width: 120,
        child: MacosTextField(
          controller: _field,
          focusNode: _focus,
          textAlign: TextAlign.end,
          keyboardType: TextInputType.number,
          inputFormatters: [FilteringTextInputFormatter.digitsOnly],
          style: text.body,
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
          suffix: entry.key.endsWith('Seconds')
              ? Padding(padding: const EdgeInsets.only(right: 6), child: Text('s', style: text.body.copyWith(color: colors.secondaryLabel)))
              : null,
          onSubmitted: (_) => _commitField(),
        ),
      ),
      engine.TextSettingValue(text: final current) when entry.choices.isNotEmpty => MacosPopupButton<String>(
        value: current,
        onChanged: (word) {
          if (word != null && word != current) _write(engine.TextSettingValue(word));
        },
        items: [for (final word in entry.choices) MacosPopupMenuItem(value: word, child: Text(_choiceTitles[word] ?? word))],
      ),
      engine.TextSettingValue() => SizedBox(
        width: 220,
        child: MacosTextField(
          controller: _field,
          focusNode: _focus,
          style: text.body,
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
          placeholder: entry.key == 'aiTagging.ompPath' ? 'From the login shell\'s PATH' : null,
          onSubmitted: (_) => _commitField(),
        ),
      ),
      _ => null,
    };
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 10, 8, 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(_titles[entry.key] ?? entry.key, style: text.body),
                    const SizedBox(height: 2),
                    Text(entry.summary, style: text.subheadline.copyWith(color: colors.secondaryLabel)),
                    Text(range == null ? entry.key : '${entry.key}  ·  $range', style: text.subheadline.copyWith(color: colors.border)),
                  ],
                ),
              ),
              const SizedBox(width: 12),
              ?control,
              SizedBox(
                width: 40,
                child: entry.changed
                    ? Center(
                        child: ToolButton(
                          tooltip: 'Reset to default',
                          icon: CupertinoIcons.arrow_counterclockwise,
                          onPressed: () => _write(null),
                        ),
                      )
                    : null,
              ),
            ],
          ),
          if (entry.value case engine.AddressesSettingValue(:final addresses))
            _Addresses(addresses: addresses, onChanged: (next) => _write(engine.AddressesSettingValue(next))),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(_error!, style: text.subheadline.copyWith(color: colors.red)),
            ),
        ],
      ),
    );
  }
}

/// A list of addresses as chips, each removable, with a field that adds more (comma-separated).
class _Addresses extends StatefulWidget {
  const _Addresses({required this.addresses, required this.onChanged});

  final List<String> addresses;
  final ValueChanged<List<String>> onChanged;

  @override
  State<_Addresses> createState() => _AddressesState();
}

class _AddressesState extends State<_Addresses> {
  final _field = TextEditingController();

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  void _add() {
    final added = _field.text.split(RegExp(r'[,\s]+')).where((a) => a.isNotEmpty);
    if (added.isEmpty) return;
    widget.onChanged([...widget.addresses, ...added]);
    _field.clear();
  }

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 8, right: 8),
    child: Wrap(
      spacing: 6,
      runSpacing: 6,
      crossAxisAlignment: WrapCrossAlignment.center,
      children: [
        for (final address in widget.addresses)
          Pill(label: address, onDelete: () => widget.onChanged([...widget.addresses]..remove(address))),
        SizedBox(
          width: 260,
          child: MacosTextField(
            controller: _field,
            style: context.text.body,
            placeholder: 'Add address',
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
            suffix: MacosIconButton(
              semanticLabel: 'Add',
              icon: MacosIcon(CupertinoIcons.plus, size: 13, color: context.colors.secondaryLabel),
              backgroundColor: MacosColors.transparent,
              hoverColor: context.colors.hover,
              padding: EdgeInsets.zero,
              boxConstraints: const BoxConstraints.tightFor(width: 22, height: 22),
              onPressed: _add,
            ),
            onSubmitted: (_) => _add(),
          ),
        ),
      ],
    ),
  );
}
