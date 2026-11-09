import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/settings.dart';

import 'tag_sync_app.dart' show TagSyncMail;

void main() {
  testWidgets('Settings syncs tags per mail account', (tester) async {
    final mail = TagSyncMail();
    final state = MailState(mail);
    addTearDown(state.dispose);
    await state.start();
    await tester.binding.setSurfaceSize(const Size(900, 1200));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(MacosApp(theme: MailTheme.macos(Brightness.light), home: SettingsWindow(state: state)));
    await tester.pump();

    expect(find.text('Sync tags with the server'), findsOneWidget);
    // Signed-in mail accounts by provider, then a listed one no longer signed in.
    expect(find.text('Work  ·  Gmail labels'), findsOneWidget);
    expect(find.text('Home  ·  Outlook categories'), findsOneWidget);
    expect(find.text('gone@example.test  ·  not signed in'), findsOneWidget);
    // No address chips: the switches stand for the list.
    expect(find.text('Add address'), findsNothing);
    bool on(String title) =>
        tester.widget<MacosSwitch>(find.descendant(of: find.widgetWithText(Row, title), matching: find.byType(MacosSwitch))).value;
    expect(on('Work  ·  Gmail labels'), isTrue);
    expect(on('Home  ·  Outlook categories'), isFalse);

    await tester.tap(find.descendant(of: find.widgetWithText(Row, 'Home  ·  Outlook categories'), matching: find.byType(MacosSwitch)));
    await tester.pump();
    expect(mail.written.last, ['gone@example.test', 'home@example.test', 'work@example.test']);
    expect(on('Home  ·  Outlook categories'), isTrue);

    await tester.tap(find.descendant(of: find.widgetWithText(Row, 'gone@example.test  ·  not signed in'), matching: find.byType(MacosSwitch)));
    await tester.pump();
    expect(mail.written.last, ['home@example.test', 'work@example.test']);
    expect(find.text('gone@example.test  ·  not signed in'), findsNothing);
  });
}
