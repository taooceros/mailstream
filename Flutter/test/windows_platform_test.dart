import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/common.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/glass.dart';
import 'package:mailstream_ui/ui/mail_html_windows.dart';
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

    testWidgets('glass capsules are drawn by Flutter on both platforms, in the app\'s light or dark', (tester) async {
      for (final mac in [true, false]) {
        for (final brightness in Brightness.values) {
          Host.debugMacOverride = mac;
          await tester.pumpWidget(MacosApp(
            key: ValueKey((mac, brightness)),
            theme: MailTheme.macos(brightness),
            home: const Center(child: GlassCapsule(child: SizedBox(width: 30, height: 30))),
          ));
          expect(find.byType(AppKitView), findsNothing);
          // No backdrop blur: it cost every frame of the reader's swipe (MS-187).
          expect(find.byType(BackdropFilter), findsNothing);
          expect(tester.getSize(find.byType(GlassCapsule)).height, GlassCapsule.height);
          final face = tester.widget<DecoratedBox>(find.descendant(of: find.byType(GlassCapsule), matching: find.byType(DecoratedBox)));
          final expected = brightness == Brightness.light ? MailColors.light.capsule : MailColors.dark.capsule;
          expect((face.decoration as ShapeDecoration).color, expected);
          expect(expected.a, 1.0);
        }
      }
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

  group('HTML bodies on Windows', () {
    test('open every link in the page, where the reader sends it to the browser', () {
      final page = windowsMailPage(
        '<html><head><base href="https://x.com/" target="_blank"></head><body>'
        '<a href="https://a.com" target="_blank">a</a><form action="https://f.com" target="_new"></form>'
        '<img src="file:///C:/cache/0-logo.png"></body></html>',
      );
      expect(page, isNot(contains('target')));
      expect(page, contains('<base href="https://x.com/">'));
      expect(page, contains('src="file:///C:/cache/0-logo.png"'));
    });

    testWidgets('are drawn by WebView2, without the AppKit view', (tester) async {
      Host.debugMacOverride = false;
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(Brightness.light),
        home: SingleChildScrollView(
          child: MailHtml(
            owner: (account: 'a', conversation: 'c', message: 'm'),
            html: '<p>Hello</p>',
            onOpenLink: (_) {},
          ),
        ),
      ));
      await tester.pump();
      expect(find.byType(WindowsMailHtml), findsOneWidget);
      expect(find.byType(AppKitView), findsNothing);
    });

    testWidgets("fold their quoted history in the page, behind the reader's toggle", (tester) async {
      Host.debugMacOverride = false;
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(Brightness.light),
        home: SingleChildScrollView(
          child: MailHtml(
            owner: (account: 'a', conversation: 'c', message: 'm'),
            html: '<p>New</p><div class="gmail_quote">Old</div>',
            onOpenLink: (_) {},
          ),
        ),
      ));
      await tester.pump();
      WindowsMailHtml body() => tester.widget<WindowsMailHtml>(find.byType(WindowsMailHtml));
      expect(find.byIcon(CupertinoIcons.ellipsis), findsNothing);
      body().onQuoted!(true);
      await tester.pump();
      expect(body().open, isFalse);
      await tester.tap(find.byIcon(CupertinoIcons.ellipsis));
      await tester.pump();
      expect(body().open, isTrue);
      expect(find.byType(WindowsMailHtml), findsOneWidget, reason: 'one page: the quotes have none of their own');
    });
  });
}
