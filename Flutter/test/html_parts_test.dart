import 'dart:async';

import 'package:flutter/cupertino.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/backend/models.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/glass.dart';
import 'package:mailstream_ui/ui/html_parts.dart';
import 'package:mailstream_ui/ui/reader.dart';

import 'reader_fixture.dart';

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

  /// A split per HTML that answers when the test completes it, recording the order they start in.
  ({List<String> calls, Map<String, Completer<HtmlParts>> answers, HtmlPartsCache cache}) held() {
    final calls = <String>[];
    final answers = <String, Completer<HtmlParts>>{};
    return (
      calls: calls,
      answers: answers,
      cache: HtmlPartsCache(split: (html) {
        calls.add(html);
        return (answers[html] = Completer<HtmlParts>()).future;
      }),
    );
  }

  test('queued work every request released is never split; the next reader runs right after the running split', () async {
    final (:calls, :answers, :cache) = held();
    final a1 = cache.request('a1');
    final a2 = cache.request('a2');
    final shared = cache.request('a3');
    final a3 = cache.request('a3');
    expect(calls, ['a1']);
    a1.release();
    a2.release();
    a3.release();
    final b = cache.request('b');
    answers['a1']!.complete(('a1', null));
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['a1', 'a3'], reason: 'a3 still has a request; a2 has none');
    shared.release();
    answers['a3']!.complete(('a3', null));
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['a1', 'a3', 'b']);
    expect(cache.peek('a1'), ('a1', null), reason: 'a running split finishes and is kept');
    answers['b']!.complete(('b', null));
    expect(await b.parts, ('b', null));
    expect(calls, ['a1', 'a3', 'b']);
  });

  test('prefetch work waits for every reader, yields to a newer batch and joins a reader asking for it', () async {
    final (:calls, :answers, :cache) = held();
    final r1 = cache.request('r1');
    cache.warm(['w1', 'w2']);
    final r2 = cache.request('r2');
    answers['r1']!.complete(('r1', null));
    await r1.parts;
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['r1', 'r2']);
    cache.warm(['w3', 'w4']);
    answers['r2']!.complete(('r2', null));
    await r2.parts;
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['r1', 'r2', 'w3'], reason: 'the newer batch replaced w1 and w2');
    final running = cache.request('w3');
    final queued = cache.request('w4');
    answers['w3']!.complete(('w3', null));
    expect(await running.parts, ('w3', null));
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['r1', 'r2', 'w3', 'w4']);
    answers['w4']!.complete(('w4', null));
    expect(await queued.parts, ('w4', null));
  });

  test("prefetched parts never push a reader's parts out", () async {
    final cache = HtmlPartsCache(split: (html) async => (html, null));
    for (var i = 0; i < HtmlPartsCache.capacity; i++) {
      await cache.load('r$i');
    }
    cache.warm([for (var i = 0; i < HtmlPartsCache.capacity; i++) 'w$i']);
    await Future<void>.delayed(Duration.zero);
    for (var i = 0; i < HtmlPartsCache.capacity; i++) {
      expect(cache.peek('r$i'), ('r$i', null));
    }
    expect(cache.peek('w0'), ('w0', null));
  });

  engine.Message message(String id, {String? html, String? text, bool unread = false, String? error}) => engine.Message(
    id: id,
    mailbox: 'INBOX',
    role: 'inbox',
    from: ['Alex <alex@example.test>'],
    to: ['reader@example.test'],
    cc: [],
    date: DateTime.utc(2026, 11, 6, 12).toIso8601String(),
    subject: 'Synthetic',
    unread: unread,
    starred: false,
    text: text,
    truncated: false,
    attachments: [],
    html: html,
    error: error,
  );

  test('a reader opens with the latest and unread HTML bodies, or a chat log with those without text', () {
    final messages = [
      message('1', html: 'old'),
      message('2', html: 'unread', unread: true),
      message('3', html: 'failed', unread: true, error: 'gone'),
      message('4', text: 'plain', unread: true),
      message('5', html: ' '),
      message('6', html: 'latest', text: 'latest as text'),
    ].map(MessageDetail.of).toList();
    Conversation of(String category) => Conversation.of(engine.Conversation(
      id: 'c',
      account: 'reader@example.test',
      subject: 'Synthetic',
      date: null,
      from: null,
      participants: [],
      messageCount: messages.length,
      unread: true,
      starred: false,
      hasAttachment: false,
      mailboxes: ['inbox'],
      snippet: '',
      category: category,
      waiting: false,
      labels: [],
    ));
    expect(Reader.openingHtml(of('notification'), messages), ['latest', 'unread']);
    expect(Reader.openingHtml(of('people'), messages), ['unread', 'old']);
  });

  /// Platform views made, oldest first; the width each asked its height at; the page's answer.
  final views = <int>[];
  final pulls = <int, Object?>{};
  late Future<Object?> Function(int view) answer;

  setUp(() {
    views.clear();
    pulls.clear();
    answer = (_) async => null;
    final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(SystemChannels.platform_views, (call) async {
      if (call.method == 'create') {
        final id = (call.arguments as Map)['id'] as int;
        views.add(id);
        // The runner's page host, which answers the height Flutter asks for once it listens.
        messenger.setMockMethodCallHandler(MethodChannel('mailstream/html/$id'), (call) async {
          pulls[id] = call.arguments;
          return answer(id);
        });
      }
      return null;
    });
  });

  const owner = (account: 'account', conversation: 'conversation', message: 'message');
  Widget host(String html, HtmlPartsCache cache, {String message = 'same'}) => MacosApp(
    theme: MailTheme.macos(Brightness.light),
    home: Center(child: MailHtml(key: ValueKey(message), owner: owner, html: html, partsCache: cache)),
  );
  List<String> rendered(WidgetTester tester) => tester.widgetList<AppKitView>(find.byType(AppKitView))
      .map((view) => (view.creationParams as Map)['html'] as String).toList();
  Finder toggle() => find.byIcon(CupertinoIcons.ellipsis);
  double height(WidgetTester tester, [int index = 0]) => tester.getSize(find.byType(AppKitView).at(index)).height;
  Future<void> measured(WidgetTester tester, int view, List<Object> height) async {
    await tester.binding.defaultBinaryMessenger.handlePlatformMessage(
      'mailstream/html/$view',
      const StandardMethodCodec().encodeMethodCall(MethodCall('height', height)),
      (_) {},
    );
    await tester.pump();
  }

  testWidgets('each part asks for its page by message and HTML, then for its height at its width', (tester) async {
    final cache = HtmlPartsCache(split: (html) async => ('main', 'quote'));
    answer = (_) async => [120.0, 3];
    await tester.pumpWidget(host('a', cache));
    await tester.pumpAndSettle();
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    final params = tester.widgetList<AppKitView>(find.byType(AppKitView)).map((view) => view.creationParams).toList();
    expect(params, [
      {'owner': ['account', 'conversation', 'message', 'body'], 'html': 'main'},
      {'owner': ['account', 'conversation', 'message', 'quote'], 'html': 'quote'},
    ]);
    expect(pulls[views.first], tester.getSize(find.byType(AppKitView).first).width);
    expect(height(tester), 120);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a late, older height cannot replace a newer one', (tester) async {
    final cache = HtmlPartsCache(split: (html) async => (html, null));
    final pull = Completer<Object?>();
    answer = (_) => pull.future;
    await tester.pumpWidget(host('a', cache));
    await tester.pumpAndSettle();
    expect(height(tester), 40);
    await measured(tester, views.single, [300.0, 4]);
    expect(height(tester), 300);
    pull.complete([120.0, 3]);
    await tester.pump();
    expect(height(tester), 300);
    await measured(tester, views.single, [90.0, 2]);
    expect(height(tester), 300);
    await measured(tester, views.single, [310.0, 5]);
    expect(height(tester), 310);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a view gone before its page answers changes nothing', (tester) async {
    final cache = HtmlPartsCache(split: (html) async => (html, null));
    final pull = Completer<Object?>();
    answer = (view) => view == views.first ? pull.future : Future.value(null);
    await tester.pumpWidget(host('a', cache, message: 'a'));
    await tester.pumpAndSettle();
    final gone = views.single;
    await tester.pumpWidget(host('b', cache, message: 'b'));
    await tester.pumpAndSettle();
    // The old page's host went with its view; its height reaches nobody.
    pull.completeError(MissingPluginException());
    await measured(tester, gone, [500.0, 9]);
    expect(tester.takeException(), isNull);
    expect(rendered(tester), ['b']);
    expect(height(tester), 40);
    await tester.pumpWidget(const SizedBox());
  });

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

  Widget bodies(HtmlPartsCache cache, Map<String, String> htmlByMessage) => MacosApp(
    theme: MailTheme.macos(Brightness.light),
    home: Column(children: [
      for (final MapEntry(key: message, value: html) in htmlByMessage.entries)
        MailHtml(key: ValueKey(message), owner: (account: 'account', conversation: 'c', message: message), html: html, partsCache: cache),
    ]),
  );

  testWidgets("a closed conversation's queued bodies are skipped; the next one splits right after the running one", (tester) async {
    final (:calls, :answers, :cache) = held();
    await tester.pumpWidget(bodies(cache, {'1': 'a1', '2': 'a2', '3': 'a3'}));
    // A refresh with new HTML releases the old.
    await tester.pumpWidget(bodies(cache, {'1': 'a1', '2': 'a2 revised', '3': 'a3'}));
    await tester.pumpWidget(bodies(cache, {'b': 'b'}));
    expect(calls, ['a1']);
    expect(find.byType(Spinner), findsOneWidget);
    answers['a1']!.complete(('a1', null));
    await tester.pump();
    expect(calls, ['a1', 'b']);
    answers['b']!.complete(('b', null));
    await tester.pump();
    expect(rendered(tester), ['b']);
    expect(calls, ['a1', 'b']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a prefetched conversation draws its bodies in its first frame; pending prefetch waits for the open one', (tester) async {
    // Prefetch splits bodies only where the reader draws HTML (the Mac's WebKit view; MS-106).
    Host.debugMacOverride = true;
    addTearDown(() => Host.debugMacOverride = null);
    final (:calls, :answers, :cache) = held();
    final mail = ReaderMail();
    final state = MailState(mail);
    addTearDown(state.dispose);
    await state.start();
    final [a, b] = state.conversations;
    void answer(int index, String html) => mail.requests[index].answer.complete(engine.Thread(
      conversation: mail.rows[index],
      messages: [message('${mail.rows[index].id}-1', html: html)],
    ));

    Reader.prefetch(state, a, partsCache: cache);
    answer(0, 'A');
    await tester.pump();
    expect(calls, ['A']);
    Reader.prefetch(state, b, partsCache: cache);
    answer(1, 'B');
    await tester.pump();
    expect(calls, ['A'], reason: 'one split at a time; B waits');

    // Another conversation opens while A is being prepared and B waits.
    await tester.pumpWidget(bodies(cache, {'c-1': 'C'}));
    expect(find.byType(Spinner), findsOneWidget);
    answers['A']!.complete(('A', null));
    await tester.pump();
    expect(calls, ['A', 'C'], reason: 'the open reader goes before pending prefetch work');
    answers['C']!.complete(('C', null));
    await tester.pump();
    expect(rendered(tester), ['C']);
    expect(calls, ['A', 'C', 'B']);
    answers['B']!.complete(('B', null));
    await tester.pump();

    for (final (message, html) in [('b-1', 'B'), ('a-1', 'A')]) {
      await tester.pumpWidget(bodies(cache, {message: html}));
      expect(find.byType(Spinner), findsNothing);
      expect(rendered(tester), [html]);
    }
    expect(calls, ['A', 'C', 'B']);
    await tester.pumpWidget(const SizedBox());
  });
}
