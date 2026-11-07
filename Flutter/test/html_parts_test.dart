import 'dart:async';

import 'package:flutter/cupertino.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/html_parts.dart';
import 'package:mailstream_ui/ui/reader.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const head = '<head><style>p { color: navy; }</style></head>';
  const quotePatterns = [
    '<div class="gmail_quote">old</div>',
    '<div class="gmail_quote_container">old</div>',
    '<div class="moz-cite-prefix">old</div>',
    '<div id="divRplyFwdMsg">old</div>',
    '<div id="appendonsend">old</div>',
    '<blockquote type="cite">old</blockquote>',
  ];

  for (final quote in quotePatterns) {
    test('background consumer preserves quote coverage: $quote', () async {
      final html = '<html>$head<body><p>new &amp; current</p>$quote</body></html>';
      final (main, quoted) = await HtmlPartsCache().load(html);
      expect(main, '<html>$head<body><p>new &amp; current</p></body></html>');
      expect(quoted, '<html>$head<body>$quote</body></html>');
    });
  }

  test('no quotes and pure quote bodies stay byte-for-byte intact', () async {
    for (final body in ['<p>unquoted &amp; unchanged</p>', ...quotePatterns, '<div> <blockquote type="cite">old</blockquote> </div>']) {
      final html = '<html>$head<body>$body</body></html>';
      expect(await HtmlPartsCache().load(html), (html, null));
    }
    expect(await HtmlPartsCache().load(''), ('', null));
  });

  test('images count as main content; nested quotes appear once in document order', () async {
    const html = '<html>$head<body><img src="cid:sample"><div class="gmail_quote"><blockquote type="cite">first</blockquote></div><div id="appendonsend">second</div></body></html>';
    final (main, quoted) = await HtmlPartsCache().load(html);
    expect(main, '<html>$head<body><img src="cid:sample"></body></html>');
    expect(quoted, '<html>$head<body><div class="gmail_quote"><blockquote type="cite">first</blockquote></div><div id="appendonsend">second</div></body></html>');
  });

  test('cache shares pending work, reuses ready results, and serializes isolates', () async {
    final calls = <String>[];
    final answers = <Completer<HtmlParts>>[];
    final cache = HtmlPartsCache(split: (html) {
      calls.add(html);
      final answer = Completer<HtmlParts>();
      answers.add(answer);
      return answer.future;
    });
    final a = cache.load('a');
    expect(identical(cache.load('a'), a), isTrue);
    final b = cache.load('b');
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['a']);
    answers[0].complete(('main a', 'quoted a'));
    expect(await a, ('main a', 'quoted a'));
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['a', 'b']);
    answers[1].complete(('main b', null));
    await b;
    expect(cache.peek('a'), ('main a', 'quoted a'));
    expect(await cache.load('a'), ('main a', 'quoted a'));
    expect(calls, ['a', 'b']);
  });

  test('LRU is bounded and failures neither poison queue nor become cached', () async {
    final calls = <String>[];
    final cache = HtmlPartsCache(split: (html) async {
      calls.add(html);
      if (html == 'bad') throw StateError('synthetic failure');
      return (html, null);
    });
    await expectLater(cache.load('bad'), throwsStateError);
    for (var i = 0; i < HtmlPartsCache.capacity; i++) {
      await cache.load('$i');
    }
    expect(cache.peek('0'), ('0', null));
    await cache.load('new');
    expect(cache.peek('1'), isNull);
    expect(cache.peek('0'), ('0', null));
    await expectLater(cache.load('bad'), throwsStateError);
    expect(calls.where((html) => html == 'bad').length, 2);
  });

  setUp(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform_views,
      (call) async => null,
    );
  });

  Widget host(String html, HtmlPartsCache cache, {String message = 'same'}) => MacosApp(
    theme: MailTheme.macos(Brightness.light),
    home: Center(child: MailHtml(key: ValueKey(message), html: html, partsCache: cache)),
  );
  List<String> rendered(WidgetTester tester) => tester.widgetList<AppKitView>(find.byType(AppKitView))
      .map((view) => (view.creationParams as Map)['html'] as String).toList();
  Finder toggle() => find.byIcon(CupertinoIcons.ellipsis);

  testWidgets('pending replacement cannot overwrite refreshed content or reopen a fold', (tester) async {
    final stale = Completer<HtmlParts>();
    final fresh = Completer<HtmlParts>();
    final firstCache = HtmlPartsCache(split: (_) => stale.future);
    final freshCache = HtmlPartsCache(split: (_) => fresh.future);
    await tester.pumpWidget(host('old', firstCache));
    expect(rendered(tester), isEmpty);
    await tester.pumpWidget(host('fresh', freshCache));
    fresh.complete(('<p>fresh</p>', '<p>fresh history</p>'));
    await tester.pumpAndSettle();
    expect(rendered(tester), ['<p>fresh</p>']);
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    expect(rendered(tester), ['<p>fresh</p>', '<p>fresh history</p>']);
    stale.complete(('<p>stale</p>', '<p>stale history</p>'));
    await tester.pumpAndSettle();
    expect(rendered(tester), ['<p>fresh</p>', '<p>fresh history</p>']);
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    expect(rendered(tester), ['<p>fresh</p>']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('refresh retains loaded body while pending and preserves the open fold', (tester) async {
    final answer = Completer<HtmlParts>();
    final cache = HtmlPartsCache(split: (html) async => html == 'a' ? ('main a', 'quote a') : answer.future);
    await tester.pumpWidget(host('a', cache));
    await tester.pumpAndSettle();
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    await tester.pumpWidget(host('b', cache));
    expect(rendered(tester), ['main a', 'quote a']);
    answer.complete(('main b', 'quote b'));
    await tester.pumpAndSettle();
    expect(rendered(tester), ['main b', 'quote b']);
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    expect(rendered(tester), ['main b']);
    await tester.pumpWidget(host('b', cache));
    await tester.pumpAndSettle();
    expect(rendered(tester), ['main b']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('quick message switching and disposal never display another message', (tester) async {
    final a = Completer<HtmlParts>();
    final b = Completer<HtmlParts>();
    final cacheA = HtmlPartsCache(split: (_) => a.future);
    final cacheB = HtmlPartsCache(split: (_) => b.future);
    await tester.pumpWidget(host('a', cacheA, message: 'a'));
    await tester.pumpWidget(host('b', cacheB, message: 'b'));
    a.complete(('main a', 'quote a'));
    await tester.pump();
    expect(rendered(tester), isEmpty);
    await tester.pumpWidget(const SizedBox());
    b.complete(('main b', null));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(host('a', cacheA, message: 'a'));
    expect(rendered(tester), ['main a']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a loaded selection is removed immediately when switching to pending mail', (tester) async {
    final pending = Completer<HtmlParts>();
    final cache = HtmlPartsCache(split: (html) async => html == 'a' ? ('main a', 'quote a') : pending.future);
    await tester.pumpWidget(host('a', cache, message: 'a'));
    await tester.pumpAndSettle();
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    expect(rendered(tester), ['main a', 'quote a']);
    await tester.pumpWidget(host('b', cache, message: 'b'));
    expect(rendered(tester), isEmpty);
    pending.complete(('main b', 'quote b'));
    await tester.pumpAndSettle();
    expect(rendered(tester), ['main b']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('no-quote and pure-quote results have no fold control', (tester) async {
    final cache = HtmlPartsCache(split: (html) async => (html, null));
    for (final html in ['<p>only main</p>', quotePatterns.last]) {
      await cache.load(html);
      await tester.pumpWidget(host(html, cache, message: html));
      expect(rendered(tester), [html]);
      expect(toggle(), findsNothing);
    }
    await tester.pumpWidget(const SizedBox());
  });
}
