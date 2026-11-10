import 'dart:convert';
import 'dart:io';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/html_parts.dart';
import 'package:mailstream_ui/ui/mail_resources.dart';
import 'package:mailstream_ui/ui/blitz_body.dart';
import 'package:mailstream_ui/ui/reader.dart';
import 'package:mailstream_ui/ui/reader_renderer.dart';
import 'package:mailstream_ui/ui/controls.dart';

/// An 80×40 PNG.
const png = 'iVBORw0KGgoAAAANSUhEUgAAAFAAAAAoCAYAAABpYH0BAAAAX0lEQVR4nO3QMQ0AIADAMKQgDWk4BQ88ZEmP/UvH3OvovfF7oB5AgADTAQQIMB1AgADTAQQIMB1AgADTAQQIMB1AgADTAQQIMB1AgADTAQQIMB1AgADTAQQIMB1AgH+7KgaFB1y8+H4AAAAASUVORK5CYII=';

void main() {
  test('only canonical raster descendants of the inline cache are readable', () async {
    final directory = await Directory.systemTemp.createTemp('ms138-resources-');
    try {
      final root = await Directory('${directory.path}/cache').create();
      final allowed = await File('${root.path}/image.png').writeAsBytes(base64Decode(png));
      final outside = await File('${directory.path}/outside.png').writeAsBytes(base64Decode(png));
      final text = await File('${root.path}/not-image.png').writeAsString('not an image');
      final link = await Link('${root.path}/escape.png').create(outside.path);
      expect(await cachedImageData(allowed.uri.toString(), cacheRoot: root.path), 'data:image/png;base64,$png');
      expect(await cachedImageData(outside.uri.toString(), cacheRoot: root.path), isNull);
      expect(await cachedImageData(Uri.file(link.path).toString(), cacheRoot: root.path), isNull);
      expect(await cachedImageData(text.uri.toString(), cacheRoot: root.path), isNull);
      expect(await cachedImageData('file://remote/image.png', cacheRoot: root.path), isNull);
    } finally {
      await directory.delete(recursive: true);
    }
  });

  test('unsupported resources and script/file links stay blocked', () async {
    final result = await prepareMailImages(
      '<img src="file:///tmp/ms138-no-read.png"><img src="http://example.test/a.png"><img src="data:text/html;base64,PHNjcmlwdD4=">',
    );
    expect(result, isNot(contains('src=')));
    for (final source in ['javascript:alert(1)', 'file:///tmp/a', 'data:text/html,a', 'https://user:password@example.test']) {
      expect(isMailLink(source), isFalse);
    }
    expect(isMailLink('https://example.com'), isTrue);
    expect(isMailLink('mailto:someone@example.test'), isTrue);
  });

  test('WebKit policy follows a leading comment and doctype, never a comment\'s <head>', () {
    const meta = '<meta http-equiv="Content-Security-Policy" content="$webKitContentPolicy">';
    expect(
      webKitDocument('<!-- <head> --><!DOCTYPE html><html><head><title>t</title></head></html>'),
      '<!-- <head> --><!DOCTYPE html>$meta<html><head><title>t</title></head></html>',
    );
    expect(webKitDocument(' <!doctype HTML>\n<p>x</p>'), ' <!doctype HTML>$meta\n<p>x</p>');
    expect(
      webKitDocument('<html><head></head><body><!DOCTYPE html></body></html>'),
      '$meta<html><head></head><body><!DOCTYPE html></body></html>',
    );
    expect(webKitContentPolicy, isNot(contains('file:')));
    expect(webKitContentPolicy, isNot(contains('script')));
  });

  test("Blitz gets the mail's presentational attributes as written, without copies (MS-196)", () async {
    const mail = '<table border="0" cellpadding="0" bgcolor="#EEEEEE"><tr align="Center"><td valign="top"><img src="https://example.test/a.png"></td></tr></table>';
    final prepared = await prepareMailImages(mail);
    for (final attribute in ['border="0"', 'cellpadding="0"', 'bgcolor="#EEEEEE"', 'align="Center"', 'valign="top"']) {
      expect(prepared, contains(attribute));
    }
    expect(prepared, isNot(contains('data-ms-')));
    // Mail without images is not parsed at all.
    expect(await prepareMailImages('<table bgcolor="#EEEEEE"><tr><td>x</td></tr></table>'), '<table bgcolor="#EEEEEE"><tr><td>x</td></tr></table>');
  });

  test('prepared mail keeps its doctype, so WebKit lays it out in the mode it was written for (MS-160)', () async {
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
      final prepared = await prepareMailImages(mail);
      expect(prepared, startsWith(expected));
      expect(splitMailHtml(prepared).$1, startsWith(expected));
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
    Widget host(ValueNotifier<ReaderRenderer> renderer, HtmlPartsCache cache, {String html = 'mail', String message = 'message'}) => MacosApp(
      theme: MailTheme.macos(Brightness.light),
      home: ReaderRendererScope(controller: renderer, child: SingleChildScrollView(child: Column(children: [
        const ReaderRendererSwitch(),
        MailHtml(key: ValueKey(message), owner: owner, html: html, onOpenLink: (_) {}, partsCache: cache),
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

    testWidgets('keeps the message and open quote, prepares once, and mounts one renderer', (tester) async {
      var prepared = 0;
      final cache = HtmlPartsCache(split: (html) async {
        prepared++;
        return ('<!DOCTYPE html><p>main</p>', '<p>history</p>');
      });
      final renderer = ValueNotifier(ReaderRenderer.blitz);
      addTearDown(renderer.dispose);
      await tester.pumpWidget(host(renderer, cache));
      await tester.pumpAndSettle();
      expect(find.text('Blitz'), findsOneWidget);
      await tester.tap(find.byIcon(CupertinoIcons.ellipsis));
      await tester.pumpAndSettle();
      // Blitz draws the prepared parts, the quote open, and nothing else is mounted.
      expect(tester.widgetList<BlitzBody>(find.byType(BlitzBody)).map((b) => b.html), ['<!DOCTYPE html><p>main</p>', '<p>history</p>']);
      expect(find.byType(AppKitView), findsNothing);

      await choose(tester, ReaderRenderer.webKit);
      expect(renderer.value, ReaderRenderer.webKit);
      expect(find.text('WebView (original)'), findsOneWidget);
      expect(find.byType(BlitzBody), findsNothing);
      // The WebKit page holds the mail as it came, one page with its quote open in it.
      expect(webDocuments(tester), [webKitDocument('mail')]);
      expect(tester.widget<AppKitView>(find.byType(AppKitView)).creationParams, containsPair('open', true));

      await choose(tester, ReaderRenderer.blitz);
      expect(renderer.value, ReaderRenderer.blitz);
      expect(tester.widgetList<BlitzBody>(find.byType(BlitzBody)).map((b) => b.html), ['<!DOCTYPE html><p>main</p>', '<p>history</p>']);
      expect(find.byType(AppKitView), findsNothing);
      expect(prepared, 1);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('Blitz never falls back to the WebView by itself', (tester) async {
      final renderer = ValueNotifier(ReaderRenderer.blitz);
      addTearDown(renderer.dispose);
      // Markup that draws poorly or not at all, then a body whose preparation fails.
      final cache = HtmlPartsCache(split: (html) async => html == 'broken'
          ? throw const FormatException('broken')
          : ('<svg><foreignObject><iframe src="https://example.test"></iframe></foreignObject></svg><center><font>x</font></center>', null));
      await tester.pumpWidget(host(renderer, cache));
      await tester.pumpAndSettle();
      expect(find.byType(BlitzBody), findsOneWidget);
      expect(find.byType(AppKitView), findsNothing);
      await tester.pumpWidget(host(renderer, cache, html: 'broken', message: 'broken'));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isA<FormatException>());
      expect(find.textContaining('Could not prepare HTML'), findsOneWidget);
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
