import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// The real shell over synthetic conversations with auto-read's settings and log kept in memory:
// a pass lands four seconds after launch and each time Settings turns auto-read on, so its
// toast, Undo and the Settings log show without accounts, private mail or settings.json.
// flutter build macos --release -t test/auto_read_app.dart
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  final mail = AutoReadDemoMail()..rows = [for (var i = 0; i < 200; i++) conversation('synthetic-$i', minute: i)];
  final state = MailState(mail);
  await state.start();
  runApp(MailstreamApp(state: state));
  Timer(const Duration(seconds: 4), mail.pass);
}

class AutoReadDemoMail extends ReaderMail {
  var _enabled = true;
  var _days = 3;
  var _batch = 0;

  static const _passes = [
    [
      ('Acme Store', 'Last chance: 40% off everything', 'newsletter', 'superseded'),
      ('Parcel Tracking', 'Your package was delivered', 'notification', 'age'),
      ('Example Bank', 'Your statement is ready', 'notification', 'age'),
    ],
    [
      ('Weekly Digest', 'This week in synthetic news', 'newsletter', 'age'),
      ('CI Robot', 'Build #4182 passed', 'notification', 'age'),
    ],
  ];

  /// One auto-read pass over made-up senders, as the engine announces it.
  void pass() {
    final picks = _passes[_batch % _passes.length];
    _batch++;
    final at = DateTime.now().toUtc().toIso8601String();
    autoRead = engine.AutoReadLog(
      enabled: _enabled,
      days: _days,
      entries: [
        for (final (index, (sender, subject, category, reason)) in picks.indexed)
          engine.AutoReadEntry(
            id: 'auto-$_batch-$index',
            account: 'reader@example.test',
            sender: sender,
            subject: subject,
            category: category,
            reason: reason,
            at: at,
            batch: _batch,
          ),
        ...autoRead.entries,
      ],
    );
    changed(engine.AutoReadEvent());
  }

  List<engine.SettingEntry> get _entries => [
    engine.SettingEntry(
      key: 'autoRead.enabled',
      value: engine.FlagSettingValue(_enabled),
      defaultValue: engine.FlagSettingValue(false),
      changed: _enabled,
      choices: const [],
      summary:
          'Mark stale notifications and newsletters (ads, promotions) read on their own, as AI tagging sorts them: unread ones older than autoRead.days, and a newsletter once its sender sends a newer one. Never mail waiting on you, starred mail, people, invitations or threads you wrote in. Mailstream Flutter offers Undo.',
    ),
    engine.SettingEntry(
      key: 'autoRead.days',
      value: engine.NumberSettingValue(_days),
      defaultValue: engine.NumberSettingValue(3),
      changed: _days != 3,
      choices: const [],
      summary: 'Days an unread notification or newsletter waits before auto-read marks it read.',
      minimum: 1,
      maximum: 90,
    ),
  ];

  @override
  Future<engine.Settings> settings() async => engine.Settings(file: '(in memory)', settings: _entries, issues: []);

  @override
  Future<engine.SettingEntry> setSetting({required String key, required engine.SettingValue value}) async {
    switch (value) {
      case engine.FlagSettingValue(:final enabled):
        _enabled = enabled;
        if (enabled) Timer(const Duration(seconds: 2), pass);
      case engine.NumberSettingValue(value: final days):
        _days = days;
      default:
        break;
    }
    return _entries.firstWhere((e) => e.key == key);
  }

  @override
  Future<engine.SettingEntry> resetSetting({required String key}) async {
    if (key == 'autoRead.enabled') _enabled = false;
    if (key == 'autoRead.days') _days = 3;
    return _entries.firstWhere((e) => e.key == key);
  }
}
