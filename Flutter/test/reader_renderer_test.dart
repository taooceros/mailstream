import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/blitz_body.dart';
import 'package:mailstream_ui/ui/mail_html_windows.dart';
import 'package:mailstream_ui/ui/reader.dart';
import 'package:mailstream_ui/ui/reader_renderer.dart';
import 'package:mailstream_ui/ui/controls.dart';

void main() {
  test("Windows' page keeps the mail's doctype, so WebView2 lays it out in the mode it was written for (MS-160)", () {
    const xhtml = '<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" '
        '"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">';
    const html4 = '<!DOCTYPE html PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">';
    final doctypes = {
      xhtml: xhtml,
      '<!doctype HTML public "-//W3C//DTD HTML 4.01 Transitional//EN">': html4,
      '<!DOCTYPE html SYSTEM "about:legacy-compat">': '<!DOCTYPE html SYSTEM "about:legacy-compat">',
      '''<!DOCTYPE html PUBLIC 'a "quoted" id'>''': '''<!DOCTYPE html PUBLIC 'a "quoted" id'>''',
      '<!DOCTYPE html>': '<!DOCTYPE html>',
    };
    for (final MapEntry(key: written, value: expected) in doctypes.entries) {
      final mail = '$written<html><head></head><body><table bgcolor="#EEEEEE"><tr><td>x'
          '<img src="https://example.test/a.png"></td></tr></table><div class="gmail_quote">history</div></body></html>';
      expect(windowsMailPage(mail), startsWith(expected));
    }
  });

  group('renderer switch', () {
    setUp(() {
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(SystemChannels.platform_views, (call) async => null);
      Host.debugMacOverride = true;
      addTearDown(() => Host.debugMacOverride = null);
    });

    const owner = (account: 'account', conversation: 'conversation', message: 'message');
    Widget host(ValueNotifier<ReaderRenderer> renderer, {String html = 'mail', String message = 'message'}) => MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: ReaderRendererScope(controller: renderer, child: SingleChildScrollView(child: Column(children: [
        const ReaderRendererSwitch(),
        MailHtml(key: ValueKey(message), owner: owner, html: html, onOpenLink: (_) {}),
      ]))),
    );
    Iterable<String> webDocuments(WidgetTester tester) => tester.widgetList<AppKitView>(find.byType(AppKitView))
        .map((view) => (view.creationParams! as Map)['html'] as String);
    Future<void> choose(WidgetTester tester, ReaderRenderer renderer) async {
      await tester.tap(find.byType(MailMenuButton<ReaderRenderer>));
      await tester.pumpAndSettle();
      await tester.tap(find.text(renderer.title).last);
      await tester.pumpAndSettle();
    }

    testWidgets('keeps the message and open quote and mounts one renderer', (tester) async {
      final renderer = ValueNotifier(ReaderRenderer.blitz);
      addTearDown(renderer.dispose);
      await tester.pumpWidget(host(renderer));
      await tester.pumpAndSettle();
      expect(find.text('Blitz'), findsOneWidget);
      tester.widget<BlitzBody>(find.byType(BlitzBody)).onQuoted!(true);
      await tester.pump();
      await tester.tap(find.byIcon(CupertinoIcons.ellipsis));
      await tester.pumpAndSettle();
      // Blitz draws the mail's two parts, the quote open, and nothing else is mounted.
      List<(String, engine.HtmlPart)> parts() =>
          tester.widgetList<BlitzBody>(find.byType(BlitzBody)).map((b) => (b.html, b.part)).toList();
      expect(parts(), [('mail', engine.HtmlPart.main), ('mail', engine.HtmlPart.quoted)]);
      expect(find.byType(AppKitView), findsNothing);

      await choose(tester, ReaderRenderer.webKit);
      expect(renderer.value, ReaderRenderer.webKit);
      expect(find.text('WebView (original)'), findsOneWidget);
      expect(find.byType(BlitzBody), findsNothing);
      // The WebKit page holds the engine's page of the mail, its quote open in it.
      expect(webDocuments(tester), [engine.mailWebDocument(html: 'mail', inline: engine.InlineParts.served)]);
      expect(tester.widget<AppKitView>(find.byType(AppKitView)).creationParams, containsPair('open', true));

      await choose(tester, ReaderRenderer.blitz);
      expect(renderer.value, ReaderRenderer.blitz);
      expect(parts(), [('mail', engine.HtmlPart.main), ('mail', engine.HtmlPart.quoted)]);
      expect(find.byType(AppKitView), findsNothing);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('Blitz never falls back to the WebView by itself', (tester) async {
      final renderer = ValueNotifier(ReaderRenderer.blitz);
      addTearDown(renderer.dispose);
      // Markup that draws poorly or not at all.
      const html = '<svg><foreignObject><iframe src="https://example.test"></iframe></foreignObject></svg><center><font>x</font></center>';
      await tester.pumpWidget(host(renderer, html: html));
      await tester.pumpAndSettle();
      expect(find.byType(BlitzBody), findsOneWidget);
      expect(find.byType(AppKitView), findsNothing);
      expect(renderer.value, ReaderRenderer.blitz);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('off the Mac there is no switch (Windows draws with WebView2)', (tester) async {
      Host.debugMacOverride = false;
      final renderer = ValueNotifier(ReaderRenderer.webKit);
      addTearDown(renderer.dispose);
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(Brightness.light),
        home: ReaderRendererScope(controller: renderer, child: const Center(child: ReaderRendererSwitch())),
      ));
      expect(find.byType(MailMenuButton<ReaderRenderer>), findsNothing);
      expect(find.textContaining('Renderer:'), findsNothing);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('the switch lists Blitz, then the WebView (MS-196)', (tester) async {
      final renderer = ValueNotifier(ReaderRenderer.initial);
      addTearDown(renderer.dispose);
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(Brightness.light),
        home: ReaderRendererScope(controller: renderer, child: const Center(child: ReaderRendererSwitch())),
      ));
      expect(find.text('Blitz'), findsOneWidget);
      final items = tester.widget<MailMenuButton<ReaderRenderer>>(find.byType(MailMenuButton<ReaderRenderer>)).items;
      expect(items.map((item) => item.value), [ReaderRenderer.blitz, ReaderRenderer.webKit]);
      expect(items.map((item) => item.label), ['Blitz', 'WebView (original)']);
      await tester.tap(find.byType(MailMenuButton<ReaderRenderer>));
      await tester.pumpAndSettle();
      await tester.tap(find.text('WebView (original)').last);
      await tester.pumpAndSettle();
      expect(renderer.value, ReaderRenderer.webKit);
      await tester.pumpWidget(const SizedBox());
    });

    test('every launch starts in Blitz unless MAILSTREAM_READER_RENDERER names another (MS-166)', () {
      expect(ReaderRenderer.fromName(null), ReaderRenderer.blitz);
      expect(ReaderRenderer.fromName('unknown'), ReaderRenderer.blitz);
      // The removed HyperRender (MS-196).
      expect(ReaderRenderer.fromName('hyper'), ReaderRenderer.blitz);
      expect(ReaderRenderer.fromName('webkit'), ReaderRenderer.webKit);
    });
  });
}
