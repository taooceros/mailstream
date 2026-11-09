import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// The real shell over synthetic conversations, signed in to a made-up Gmail and Outlook
// account, with `aiTagging.syncAccounts` kept in memory: Settings (⌘,) ▸ AI tagging shows a
// switch per account, and one listed account that is no longer signed in. No accounts, private
// mail or settings.json.
// flutter build macos --release -t test/tag_sync_app.dart
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final mail = TagSyncMail()..rows = [for (var i = 0; i < 50; i++) conversation('synthetic-$i', minute: i)];
  final state = MailState(mail);
  await state.start();
  runApp(MailstreamApp(state: state));
}

class TagSyncMail extends ReaderMail {
  TagSyncMail({List<String> synced = const ['gone@example.test', 'work@example.test']}) : _synced = [...synced];

  List<String> _synced;
  final written = <List<String>>[];

  static const _summary =
      'Accounts whose AI tags sync with the server, both ways, as Gmail labels or Outlook categories named People, Notifications, Newsletters, Invitations and Waiting on you (an existing label of that name is used, in any case). Mail another device or the server already tagged is not tagged again here. Labels are never deleted.';

  @override
  Future<List<engine.Account>> accounts() async => [
    engine.Account(id: 'Work@example.test', label: 'Work', provider: 'google', services: ['mail', 'chat']),
    engine.Account(id: 'home@example.test', label: 'Home', provider: 'microsoft', services: ['mail']),
  ];

  engine.SettingEntry get _entry => engine.SettingEntry(
    key: 'aiTagging.syncAccounts',
    value: engine.AddressesSettingValue(_synced),
    defaultValue: engine.AddressesSettingValue(const []),
    changed: _synced.isNotEmpty,
    choices: const [],
    summary: _summary,
  );

  @override
  Future<engine.Settings> settings() async => engine.Settings(
    file: '(in memory)',
    settings: [
      engine.SettingEntry(
        key: 'aiTagging.enabled',
        value: engine.FlagSettingValue(true),
        defaultValue: engine.FlagSettingValue(true),
        changed: false,
        choices: const [],
        summary: 'Sort the inbox into People, Notifications, Newsletters and Invitations with AI, and flag mail waiting on your reply or action.',
      ),
      _entry,
    ],
    issues: [],
  );

  @override
  Future<engine.SettingEntry> setSetting({required String key, required engine.SettingValue value}) async {
    if (value case engine.AddressesSettingValue(:final addresses)) {
      _synced = ([...addresses]..sort()).toSet().toList();
      written.add(_synced);
    }
    return _entry;
  }

  @override
  Future<engine.SettingEntry> resetSetting({required String key}) async {
    _synced = [];
    return _entry;
  }
}
