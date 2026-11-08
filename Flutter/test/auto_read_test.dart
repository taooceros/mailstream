import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/settings.dart';

import 'reader_fixture.dart';

engine.AutoReadEntry entry(String id, String category, int batch, {String reason = 'age'}) => engine.AutoReadEntry(
  id: id,
  account: 'reader@example.test',
  sender: 'Sender $id',
  subject: 'Subject $id',
  category: category,
  reason: reason,
  at: DateTime.utc(2026, 11, 8, 10).toIso8601String(),
  batch: batch,
);

engine.SettingEntry setting(String key, engine.SettingValue value) => engine.SettingEntry(
  key: key,
  value: value,
  defaultValue: value,
  changed: false,
  choices: const [],
  summary: 'Synthetic $key.',
);

/// [ReaderMail] with auto-read on in its settings.
class AutoReadMail extends ReaderMail {
  @override
  Future<engine.Settings> settings() async => engine.Settings(
    file: '(in memory)',
    settings: [
      setting('autoRead.enabled', engine.FlagSettingValue(true)),
      setting('autoRead.days', engine.NumberSettingValue(3)),
    ],
    issues: [],
  );
}

void main() {
  testWidgets('a new auto-read pass toasts once with Undo; passes from before the launch stay quiet', (tester) async {
    final mail = ReaderMail()..autoRead = engine.AutoReadLog(enabled: true, days: 3, entries: [entry('old', 'notification', 1)]);
    final state = MailState(mail);
    addTearDown(state.dispose);
    await state.start();
    await tester.pump();
    expect(state.toast, isNull);
    expect(state.autoRead!.entries.map((e) => e.id), ['old']);

    mail.autoRead = engine.AutoReadLog(
      enabled: true,
      days: 3,
      entries: [entry('n2', 'newsletter', 2, reason: 'superseded'), entry('a1', 'notification', 2), entry('old', 'notification', 1)],
    );
    mail.changed(engine.AutoReadEvent());
    await tester.pump();
    final shown = state.toast!;
    expect(shown.text, 'Marked 1 notification and 1 newsletter read as stale');
    expect(shown.action!.label, 'Undo');
    // Read again, the same pass shows no second toast.
    mail.changed(engine.AutoReadEvent());
    await tester.pump();
    expect(state.toast, same(shown));

    shown.action!.onPressed();
    await tester.pump();
    expect(mail.undone, [
      ['n2', 'a1'],
    ]);
    expect(state.toast!.text, 'Marked 2 conversations unread again');
    expect(state.autoRead!.entries.map((e) => e.id), ['old']);
  });

  test('the toast counts each kind', () {
    expect(autoReadSummary([entry('a', 'notification', 1)]), 'Marked 1 notification read as stale');
    expect(autoReadSummary([entry('a', 'newsletter', 1), entry('b', 'newsletter', 1)]), 'Marked 2 newsletters read as stale');
  });

  testWidgets('Settings lists what auto-read marked, each with Mark Unread', (tester) async {
    final mail = AutoReadMail()
      ..autoRead = engine.AutoReadLog(
        enabled: true,
        days: 3,
        entries: [entry('n2', 'newsletter', 2, reason: 'superseded'), entry('a1', 'notification', 2)],
      );
    final state = MailState(mail);
    addTearDown(state.dispose);
    await state.start();
    await tester.binding.setSurfaceSize(const Size(900, 900));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(MacosApp(theme: MailTheme.macos(Brightness.light), home: SettingsWindow(state: state)));
    await tester.pump();
    expect(find.text('Auto-read'), findsOneWidget);
    expect(find.text('Mark stale notifications and newsletters read'), findsOneWidget);
    await tester.scrollUntilVisible(find.text('Recently marked read'), 200, scrollable: find.byType(Scrollable).first);
    expect(find.textContaining('Subject n2'), findsOneWidget);
    expect(find.textContaining('a newer one from the same sender arrived'), findsOneWidget);
    expect(find.textContaining('unread too long'), findsOneWidget);

    await tester.tap(find.text('Mark All Unread'));
    await tester.pump();
    expect(mail.undone, [
      ['n2', 'a1'],
    ]);
    await tester.pump();
    expect(find.textContaining('Subject n2'), findsNothing);
    expect(find.textContaining('Nothing yet. Unread notifications and newsletters older than 3 days'), findsOneWidget);
  });
}
