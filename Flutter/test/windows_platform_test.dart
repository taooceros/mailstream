import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/common.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/glass.dart';
import 'package:mailstream_ui/ui/html_parts.dart';
import 'package:mailstream_ui/ui/reader.dart';

/// The Windows branches of the Mac-first UI, taken on any host through [Host.debugMacOverride].
void main() {
  tearDown(() => Host.debugMacOverride = null);

  group('command shortcuts', () {
    test('read ⌘ on the Mac and Ctrl on Windows', () {
      Host.debugMacOverride = true;
      expect(Host.shortcut('W'), '⌘W');
      expect(Host.shortcut('O', shift: true), '⌘⇧O');
      expect(Host.shortcut('Return'), '⌘ Return');
      expect(Host.deleteKey, '⌫');
      Host.debugMacOverride = false;
      expect(Host.shortcut('W'), 'Ctrl+W');
      expect(Host.shortcut('O', shift: true), 'Ctrl+Shift+O');
      expect(Host.shortcut('Return'), 'Ctrl+Enter');
      expect(Host.shortcut(','), 'Ctrl+,');
      expect(Host.deleteKey, 'Del');
    });

    test('bind ⌘ on the Mac and Ctrl on Windows', () {
      Host.debugMacOverride = true;
      final mac = Host.command(LogicalKeyboardKey.keyO, shift: true);
      expect((mac.meta, mac.control, mac.shift), (true, false, true));
      Host.debugMacOverride = false;
      final windows = Host.command(LogicalKeyboardKey.keyW);
      expect((windows.meta, windows.control, windows.shift), (false, true, false));
    });

    testWidgets('Ctrl+W fires a Windows binding; ⌘W and AltGr do not', (tester) async {
      Host.debugMacOverride = false;
      var closes = 0;
      final focus = FocusNode();
      addTearDown(focus.dispose);
      await tester.pumpWidget(
        CallbackShortcuts(
          bindings: {Host.command(LogicalKeyboardKey.keyW): () => closes++},
          child: Focus(focusNode: focus, autofocus: true, child: const SizedBox()),
        ),
      );
      await tester.pump();
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      expect(Host.commandPressed, isTrue);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyW);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      expect(closes, 1);

      await tester.sendKeyDownEvent(LogicalKeyboardKey.metaLeft);
      expect(Host.commandPressed, isFalse);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyW);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.metaLeft);
      expect(closes, 1);

      // AltGr arrives as Ctrl+Alt: it types characters, it is not a command.
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyDownEvent(LogicalKeyboardKey.altRight);
      expect(Host.commandPressed, isFalse);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.altRight);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    });
  });

  group('window chrome off the Mac', () {
    Widget host(Widget child) => MacosApp(theme: MailTheme.macos(Brightness.light), home: Center(child: child));

    testWidgets('glass capsules are drawn by Flutter, without the AppKit view', (tester) async {
      Host.debugMacOverride = false;
      await tester.pumpWidget(host(const GlassCapsule(child: SizedBox(width: 30, height: 30))));
      expect(find.byType(AppKitView), findsNothing);
      expect(find.byType(BackdropFilter), findsOneWidget);
      expect(tester.getSize(find.byType(GlassCapsule)).height, GlassCapsule.height);
    });
    testWidgets('the title bar area leaves dragging to the system title bar', (tester) async {
      final calls = <String>[];
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(const MethodChannel('mailstream/window'), (call) async {
        calls.add(call.method);
        return null;
      });
      const area = WindowDragArea(child: ColoredBox(color: Color(0x01000000), child: SizedBox(width: 200, height: 40)));
      Host.debugMacOverride = false;
      await tester.pumpWidget(host(area));
      await tester.drag(find.byType(WindowDragArea), const Offset(40, 0));
      expect(calls, isEmpty);
      // The Mac's window has no title bar of its own: the area asks the runner to drag.
      Host.debugMacOverride = true;
      await tester.pumpWidget(host(KeyedSubtree(key: const ValueKey('mac'), child: area)));
      await tester.drag(find.byType(WindowDragArea), const Offset(40, 0));
      expect(calls, ['drag']);
      await tester.pump(const Duration(seconds: 1));
    });
  });

  group('HTML bodies without an HTML view', () {
    Widget host(Widget child) => MacosApp(theme: MailTheme.macos(Brightness.light), home: SingleChildScrollView(child: child));

    testWidgets('show the message text under the notice', (tester) async {
      await tester.pumpWidget(host(HtmlTextFallback(
        text: 'Hello from the text part.',
        html: '<p>Hello</p>',
        toText: (_) => fail('the text part is used as it is'),
      )));
      expect(find.text(HtmlTextFallback.notice), findsOneWidget);
      expect(find.text('Hello from the text part.'), findsOneWidget);
      expect(find.byType(AppKitView), findsNothing);
    });

    testWidgets('read the HTML as text when the message has none', (tester) async {
      final derived = Completer<String>();
      await tester.pumpWidget(host(HtmlTextFallback(text: '', html: '<p>Only HTML</p>', toText: (_) => derived.future)));
      expect(find.text(HtmlTextFallback.notice), findsOneWidget);
      expect(find.byType(ProgressCircle), findsOneWidget);
      derived.complete('Only HTML');
      await tester.pump();
      expect(find.text('Only HTML'), findsOneWidget);
    });

    testWidgets('say so when the HTML has no text either', (tester) async {
      await tester.pumpWidget(host(HtmlTextFallback(text: null, html: '<img src="x">', toText: (_) async => '  ')));
      await tester.pump();
      expect(find.text('This message has no text.'), findsOneWidget);
    });

    test('HTML reads as its visible text, a line per block', () {
      final text = mailHtmlText(
        '<html><head><title>T</title><style>p{}</style></head><body>'
        '<p>First<br>line two</p><div>Second</div><script>alert(1)</script>'
        '<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table></body></html>',
      );
      expect(text.split('\n').map((l) => l.trim()).where((l) => l.isNotEmpty), ['First', 'line two', 'Second', 'ab', 'c']);
    });
  });
}
