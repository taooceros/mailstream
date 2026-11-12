import 'dart:async';

import 'package:flutter/cupertino.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/blitz_body.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/reader.dart';
import 'package:mailstream_ui/ui/reader_renderer.dart';

/// The reader's HTML bodies over the engine's rules (`FFI/src/mail_html.rs`): the Mac's WebKit
/// pages and the Blitz bodies' folds. Windows' WebView2 bodies: windows_platform_test.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// Platform views made, oldest first; the width each asked its height at; the page's answer;
  /// a height each page sends while its view is being made, before Flutter hears of the view;
  /// the fold calls each page got.
  final views = <int>[];
  final pulls = <int, Object?>{};
  final folds = <(int, bool)>[];
  late Future<Object?> Function(int view) answer;
  List<Object>? Function(int view) early = (_) => null;

  /// The renderer the bodies below draw with; WebKit-only tests leave it on WebKit.
  var renderer = ReaderRenderer.webKit;

  Future<void> send(int view, List<Object> height) => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
      .handlePlatformMessage(
        'mailstream/html',
        const StandardMethodCodec().encodeMethodCall(MethodCall('height', [view, ...height])),
        (_) {},
      );

  setUp(() {
    renderer = ReaderRenderer.webKit;
    // The WebKit view path is the Mac's (Windows draws WebView2 bodies: windows_platform_test).
    Host.debugMacOverride = true;
    addTearDown(() => Host.debugMacOverride = null);
    views.clear();
    pulls.clear();
    folds.clear();
    answer = (_) async => null;
    early = (_) => null;
    final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    // The runner's pages, which answer the height a view asks for once it listens.
    messenger.setMockMethodCallHandler(const MethodChannel('mailstream/html'), (call) async {
      final arguments = call.arguments as Map;
      final view = arguments['view'] as int;
      if (call.method == 'fold') {
        folds.add((view, arguments['open'] as bool));
        return null;
      }
      pulls[view] = arguments['width'];
      return answer(view);
    });
    messenger.setMockMethodCallHandler(SystemChannels.platform_views, (call) async {
      if (call.method == 'create') {
        final id = (call.arguments as Map)['id'] as int;
        views.add(id);
        if (early(id) case final height?) await send(id, height);
      }
      return null;
    });
  });

  const owner = (account: 'account', conversation: 'conversation', message: 'message');
  Widget scoped(Widget child) => MacosApp(
    theme: MailTheme.macos(Brightness.light),
    home: ReaderRendererScope(controller: ValueNotifier(renderer), child: child),
  );
  Widget host(String html, {String message = 'same'}) =>
      scoped(Center(child: MailHtml(key: ValueKey(message), owner: owner, html: html, onOpenLink: (_) {})));
  Widget bodies(Map<String, String> htmlByMessage) => scoped(Column(children: [
    for (final MapEntry(key: message, value: html) in htmlByMessage.entries)
      MailHtml(key: ValueKey(message), owner: (account: 'account', conversation: 'c', message: message), html: html, onOpenLink: (_) {}),
  ]));

  /// The HTML each mounted body draws; only the selected renderer's bodies are mounted.
  List<String> rendered(WidgetTester tester) {
    expect(find.byType(renderer == ReaderRenderer.blitz ? AppKitView : BlitzBody), findsNothing);
    final policy = engine.mailWebDocument(html: '', inline: engine.InlineParts.served);
    return [
      for (final view in tester.widgetList<AppKitView>(find.byType(AppKitView)))
        ((view.creationParams as Map)['html'] as String).replaceFirst(policy, ''),
      for (final body in tester.widgetList<BlitzBody>(find.byType(BlitzBody))) body.html,
    ];
  }
  Finder toggle() => find.byIcon(CupertinoIcons.ellipsis);
  double height(WidgetTester tester, [int index = 0]) => tester.getSize(find.byType(AppKitView).at(index)).height;
  Future<void> measured(WidgetTester tester, int view, List<Object> height) async {
    await send(view, height);
    await tester.pump();
  }

  testWidgets("the WebKit body loads the engine's page of the mail with the engine's script, then asks for its height", (tester) async {
    final inlineCacheUrl = Uri.directory(engine.inlinePartsFolder()).toString();
    answer = (_) async => [120.0, 3, false];
    // Exactly what the engine hands over: a doctype after a comment, an Outlook conditional
    // comment, srcset, an http image, and an inline image as a file in the inline cache.
    final mail = '<!-- engine --><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" '
        '"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd"><html><head><!--[if mso]><style>x{}</style><![endif]-->'
        '</head><body bgcolor="#eeeeee"><img src="https://example.test/a.png" srcset="https://example.test/a@2x.png 2x">'
        '<img src="http://example.test/b.png"><img src="${inlineCacheUrl}0a1b/0-logo%20a.png">'
        '<div class="gmail_quote">old</div></body></html>';
    await tester.pumpWidget(host(mail));
    await tester.pumpAndSettle();
    final params = tester.widgetList<AppKitView>(find.byType(AppKitView)).map((view) => view.creationParams).toList();
    final meta = engine.mailWebDocument(html: '', inline: engine.InlineParts.served);
    const head = '<!-- engine --><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" '
        '"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">';
    final page = '$head$meta${mail.substring(head.length).replaceFirst('${inlineCacheUrl}0a1b/', 'mscid://0a1b/')}';
    expect(params, [
      {
        'owner': ['account', 'conversation', 'message'],
        'html': page,
        'script': engine.mailWebScript(view: engine.MailWebView.webKit),
        'inlineCache': engine.inlinePartsFolder(),
        'open': false,
      },
    ]);
    // Byte for byte the engine's mail but for the policy and the inline image's address.
    expect(page.replaceFirst(meta, '').replaceFirst('mscid://0a1b/', '${inlineCacheUrl}0a1b/'), mail);
    expect(pulls[views.single], tester.getSize(find.byType(AppKitView)).width);
    expect(height(tester), 120);
    expect(toggle(), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets("a link clicked in a WebKit page opens only if the engine's rule allows", (tester) async {
    final opened = <String>[];
    final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(const MethodChannel('mailstream/window'), (call) async {
      if (call.method == 'openMailLink') opened.add(call.arguments as String);
      return null;
    });
    addTearDown(() => messenger.setMockMethodCallHandler(const MethodChannel('mailstream/window'), null));
    await tester.pumpWidget(host('<a href="https://example.test">x</a>'));
    await tester.pumpAndSettle();
    for (final link in ['https://example.test/a', 'file:///etc/passwd', 'mailto:a@example.test', 'javascript:alert(1)']) {
      await messenger.handlePlatformMessage(
        'mailstream/html',
        const StandardMethodCodec().encodeMethodCall(MethodCall('link', [views.single, link])),
        (_) {},
      );
    }
    expect(opened, ['https://example.test/a', 'mailto:a@example.test']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('quoted history folds in the page: the toggle shows once the page finds quotes, and opens them there', (tester) async {
    await tester.pumpWidget(host('<p>new</p><div class="gmail_quote">old</div>'));
    await tester.pumpAndSettle();
    expect(toggle(), findsNothing);
    await measured(tester, views.single, [200.0, 1, true]);
    expect(toggle(), findsOneWidget);
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    expect(folds, [(views.single, true)]);
    await measured(tester, views.single, [320.0, 2, true]);
    expect(height(tester), 320);
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    expect(folds, [(views.single, true), (views.single, false)]);
    // Still one view: the quote has no page of its own.
    expect(find.byType(AppKitView), findsOneWidget);
    // A refresh's page starts with the fold as the reader keeps it.
    await tester.tap(toggle());
    await tester.pumpAndSettle();
    await tester.pumpWidget(host('<p>new, edited</p><div class="gmail_quote">old</div>'));
    await tester.pumpAndSettle();
    expect(tester.widget<AppKitView>(find.byType(AppKitView)).creationParams, containsPair('open', true));
    expect(toggle(), findsNothing, reason: 'the new page has not said it has quotes yet');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a late, older height cannot replace a newer one', (tester) async {
    final pull = Completer<Object?>();
    answer = (_) => pull.future;
    await tester.pumpWidget(host('a'));
    await tester.pumpAndSettle();
    expect(height(tester), 40);
    await measured(tester, views.single, [300.0, 4, false]);
    expect(height(tester), 300);
    pull.complete([120.0, 3, false]);
    await tester.pump();
    expect(height(tester), 300);
    await measured(tester, views.single, [90.0, 2, false]);
    expect(height(tester), 300);
    await measured(tester, views.single, [310.0, 5, false]);
    expect(height(tester), 310);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a view gone before its page answers changes nothing', (tester) async {
    final pull = Completer<Object?>();
    answer = (view) => view == views.first ? pull.future : Future.value(null);
    await tester.pumpWidget(host('a', message: 'a'));
    await tester.pumpAndSettle();
    final gone = views.single;
    await tester.pumpWidget(host('b', message: 'b'));
    await tester.pumpAndSettle();
    // The old page's host went with its view; its height reaches nobody.
    pull.completeError(MissingPluginException());
    await measured(tester, gone, [500.0, 9, true]);
    expect(tester.takeException(), isNull);
    expect(rendered(tester), ['b']);
    expect(height(tester), 40);
    expect(toggle(), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('every body shares one channel, each height reaching its own view', (tester) async {
    await tester.pumpWidget(bodies({'1': 'first', '2': 'second'}));
    await tester.pumpAndSettle();
    final [first, second] = views;
    expect(pulls.keys, [first, second]);
    await measured(tester, second, [210.0, 1, false]);
    await measured(tester, first, [330.0, 1, false]);
    expect([height(tester, 0), height(tester, 1)], [330, 210]);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a height sent while its view was being made reaches the body once it listens', (tester) async {
    early = (_) => [260.0, 1, false];
    await tester.pumpWidget(host('a'));
    await tester.pumpAndSettle();
    expect(height(tester), 260);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('a Blitz body draws the part before the quotes; the toggle shows once its page finds quotes', (tester) async {
    renderer = ReaderRenderer.blitz;
    const html = '<p>new</p><div class="gmail_quote">old</div>';
    await tester.pumpWidget(host(html));
    final main = tester.widget<BlitzBody>(find.byType(BlitzBody));
    expect(main.part, engine.HtmlPart.main);
    expect(toggle(), findsNothing);
    main.onQuoted!(true);
    await tester.pump();
    expect(toggle(), findsOneWidget);
    expect(find.byType(BlitzBody), findsOneWidget, reason: 'the quotes page opens with the toggle');
    await tester.tap(toggle());
    await tester.pump();
    final parts = tester.widgetList<BlitzBody>(find.byType(BlitzBody)).map((body) => (body.html, body.part)).toList();
    expect(parts, [(html, engine.HtmlPart.main), (html, engine.HtmlPart.quoted)]);
    await tester.pumpWidget(const SizedBox());
  });
}
