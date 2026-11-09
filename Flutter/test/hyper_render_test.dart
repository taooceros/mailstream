import 'dart:convert';
import 'dart:io';

import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/material.dart' show MaterialApp;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hyper_render/hyper_render.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/html_parts.dart';
import 'package:mailstream_ui/ui/hyper_body.dart';
import 'package:mailstream_ui/ui/hyper_resources.dart';
import 'package:mailstream_ui/ui/hyper_table.dart';
import 'package:mailstream_ui/ui/blitz_body.dart';
import 'package:mailstream_ui/ui/reader.dart';
import 'package:mailstream_ui/ui/reader_renderer.dart';

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
    final result = await prepareHyperResources(
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

  test('presentational attributes the sanitizer drops are kept as inert data-ms copies', () async {
    final prepared = await prepareHyperResources(
      '<table border="0" cellpadding="0" bgcolor="#EEEEEE"><tr align="Center"><td valign="top">x</td></tr></table>',
    );
    for (final copy in ['data-ms-border="0"', 'data-ms-cellpadding="0"', 'data-ms-bgcolor="#eeeeee"', 'data-ms-align="center"', 'data-ms-valign="top"']) {
      expect(prepared, contains(copy));
    }
    expect(prepared, contains('align="Center"'));
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
      final prepared = await prepareHyperResources(mail);
      expect(prepared, startsWith(expected));
      expect(splitMailHtml(prepared).$1, startsWith(expected));
      expect(windowsMailPage(mail), startsWith(expected));
    }
  });

  testWidgets('the image builder reaches nested table cells without a WebView', (tester) async {
    final seen = <String>[];
    await tester.pumpWidget(MaterialApp(home: SingleChildScrollView(child: HyperViewer(
      html: '<table><tr><td><table><tr><td><img src="data:image/png;base64,$png" width="80" height="40"></td></tr></table></td></tr></table>',
      mode: HyperRenderMode.sync,
      shrinkWrap: true,
      physics: const NeverScrollableScrollPhysics(),
      widgetBuilder: (node) {
        if (node.tagName == 'img') seen.add((node as AtomicNode).src ?? '');
        return HyperBody.imageWidget(node);
      },
      imageLoader: HyperBody.loadImage,
    ))));
    await tester.pump(const Duration(milliseconds: 300));
    expect(seen, contains('data:image/png;base64,$png'));
    expect(find.byType(Image), findsOneWidget);
    expect(find.byType(AppKitView), findsNothing);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('mail layout tables fit the column and draw grid lines only for border="1"', (tester) async {
    tester.view.physicalSize = const Size(400, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final html = await tester.runAsync(() => prepareHyperResources(
      '<table width="600" border="0" cellpadding="0"><tr><td>${'Wrapping words in a fixed 600 pixel layout table. ' * 6}</td></tr></table>'
      '<table border="1"><tr><td>A</td><td>B</td></tr></table>'
      '<table width="50%" border="0"><tr><td>Half</td></tr></table>',
    ));
    await tester.pumpWidget(MaterialApp(home: SingleChildScrollView(child: HyperBody(html: html!))));
    await tester.pump(const Duration(milliseconds: 300));
    final tables = tester.widgetList<HyperTable>(find.byType(HyperTable)).toList();
    expect(tables.map((t) => t.borderWidth), [0, 1, 0]);
    expect(tables.every((t) => t.onLinkTap != null), isTrue, reason: 'links in inline-only cells open');
    final sizes = [for (final table in find.byType(HyperTable).evaluate()) (table.renderObject! as RenderBox).size.width];
    expect(sizes[0], lessThanOrEqualTo(400));
    // An auto-width table keeps its natural width; a 50% one takes half the column, once.
    expect(sizes[1], lessThan(100));
    expect(sizes[2], closeTo(200, 1));
    expect(find.descendant(of: find.byType(MailTable), matching: find.byType(SingleChildScrollView)), findsNothing);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
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
      await tester.tap(find.byType(MacosPopupButton<ReaderRenderer>));
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
      final renderer = ValueNotifier(ReaderRenderer.hyper);
      addTearDown(renderer.dispose);
      await tester.pumpWidget(host(renderer, cache));
      await tester.pumpAndSettle();
      expect(find.text('HyperRender'), findsOneWidget);
      await tester.tap(find.byIcon(CupertinoIcons.ellipsis));
      await tester.pumpAndSettle();
      expect(find.byType(HyperBody), findsNWidgets(2));
      expect(find.byType(AppKitView), findsNothing);

      await choose(tester, ReaderRenderer.webKit);
      expect(renderer.value, ReaderRenderer.webKit);
      expect(find.text('WebView (original)'), findsOneWidget);
      expect(find.byType(HyperBody), findsNothing);
      const meta = '<meta http-equiv="Content-Security-Policy" content="$webKitContentPolicy">';
      // The quote is still open: both parts are on screen, each under the content policy.
      expect(webDocuments(tester), ['<!DOCTYPE html>$meta<p>main</p>', '$meta<p>history</p>']);

      await choose(tester, ReaderRenderer.blitz);
      expect(renderer.value, ReaderRenderer.blitz);
      // Blitz draws the same prepared parts, the quote still open, and nothing else is mounted.
      expect(tester.widgetList<BlitzBody>(find.byType(BlitzBody)).map((b) => b.html), ['<!DOCTYPE html><p>main</p>', '<p>history</p>']);
      expect(find.byType(HyperBody), findsNothing);
      expect(find.byType(AppKitView), findsNothing);

      await choose(tester, ReaderRenderer.hyper);
      expect(find.byType(HyperBody), findsNWidgets(2));
      expect(find.byType(BlitzBody), findsNothing);
      expect(find.byType(AppKitView), findsNothing);
      expect(prepared, 1);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('HyperRender never falls back to the WebView by itself', (tester) async {
      final renderer = ValueNotifier(ReaderRenderer.hyper);
      addTearDown(renderer.dispose);
      // Markup HyperRender draws poorly or not at all, then a body whose preparation fails.
      final cache = HtmlPartsCache(split: (html) async => html == 'broken'
          ? throw const FormatException('broken')
          : ('<svg><foreignObject><iframe src="https://example.test"></iframe></foreignObject></svg><center><font>x</font></center>', null));
      await tester.pumpWidget(host(renderer, cache));
      await tester.pumpAndSettle();
      expect(find.byType(HyperBody), findsOneWidget);
      expect(find.byType(AppKitView), findsNothing);
      await tester.pumpWidget(host(renderer, cache, html: 'broken', message: 'broken'));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isA<FormatException>());
      expect(find.textContaining('Could not prepare HTML'), findsOneWidget);
      expect(find.byType(AppKitView), findsNothing);
      expect(renderer.value, ReaderRenderer.hyper);
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
      expect(find.byType(MacosPopupButton<ReaderRenderer>), findsNothing);
      expect(find.textContaining('Renderer:'), findsNothing);
      await tester.pumpWidget(const SizedBox());
    });

    test('every launch starts in Blitz unless MAILSTREAM_READER_RENDERER names another (MS-166)', () {
      expect(ReaderRenderer.fromName(null), ReaderRenderer.blitz);
      expect(ReaderRenderer.fromName('unknown'), ReaderRenderer.blitz);
      expect(ReaderRenderer.fromName('hyper'), ReaderRenderer.hyper);
      expect(ReaderRenderer.fromName('webkit'), ReaderRenderer.webKit);
    });
  });
}
