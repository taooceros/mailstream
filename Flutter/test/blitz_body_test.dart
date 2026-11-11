import 'dart:async';

import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/blitz_body.dart';

/// A page 3000 points tall whose first 100 points are a link.
class FakePage implements engine.HtmlPageInterface {
  final tops = <double>[];
  final widths = <double>[];
  final selections = <List<double>>[];
  var closed = false;
  final changes = StreamController<int>.broadcast();

  @override
  Future<engine.HtmlSize?> layout({required double width, required double scale}) async {
    widths.add(width);
    return engine.HtmlSize(height: 3000, version: 0);
  }

  @override
  Future<engine.HtmlTile?> tile({required double width, required double scale, required double top, required double height}) async {
    tops.add(top);
    final (w, h) = ((width * scale).round(), (height * scale).round());
    return engine.HtmlTile(width: w, height: h, pixels: Uint8List(w * h * 4));
  }

  @override
  Future<String?> linkAt({required double x, required double y}) async => y < 100 ? 'https://example.com/a' : null;

  @override
  Future<bool> select({required double fromX, required double fromY, required double toX, required double toY}) async {
    selections.add([fromX, fromY, toX, toY]);
    return true;
  }

  @override
  Future<bool> clearSelection() async => true;

  @override
  Future<String?> selectedText() async => 'Alpha beta';

  @override
  Future<String?> text() async => 'Alpha beta gamma';

  @override
  Future<int?> changed({required int version}) => changes.stream.first.then<int?>((v) => v, onError: (_) => null);

  @override
  void close() {
    closed = true;
    changes.close();
  }
}

void main() {
  late FakePage page;
  late List<String> opened;
  late ScrollController scroll;

  Widget app({bool show = true}) => Directionality(
    textDirection: TextDirection.ltr,
    child: MediaQuery(
      data: const MediaQueryData(size: Size(400, 600), devicePixelRatio: 2),
      child: SingleChildScrollView(
        controller: scroll,
        child: show ? BlitzBody(html: '<p>x</p>', page: (_) => page, openLink: opened.add) : const SizedBox(height: 10),
      ),
    ),
  );

  /// Lets the fake's answers and the pixel decoding (real async work) finish.
  Future<void> settle(WidgetTester tester) async {
    for (var i = 0; i < 4; i++) {
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
      await tester.pump();
    }
  }

  setUp(() {
    page = FakePage();
    opened = [];
    scroll = ScrollController();
  });

  testWidgets('the body is as tall as the page and draws only the tiles near the viewport', (tester) async {
    tester.view.physicalSize = const Size(800, 1200);
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(app());
    await settle(tester);
    expect(tester.getSize(find.byType(BlitzBody)).height, 3000);
    expect(page.widths, [400]);
    // 600 points of viewport and a tile ahead: tiles 0, 1 and 2.
    expect(page.tops.toSet(), {0, 512, 1024});

    scroll.jumpTo(2000);
    await settle(tester);
    expect(page.tops.toSet(), containsAll(<double>[1536, 2048, 2560]));
    expect(page.tops.where((top) => top > 2560), isEmpty);
  });

  testWidgets('a click opens the link under it; a drag selects and ⌘C copies', (tester) async {
    final copied = <String>[];
    // ⌘C is the Mac's copy (Ctrl+C elsewhere); the Mac's branch on any host.
    Host.debugMacOverride = true;
    addTearDown(() => Host.debugMacOverride = null);
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
      if (call.method == 'Clipboard.setData') copied.add((call.arguments as Map)['text'] as String);
      return null;
    });
    await tester.pumpWidget(app());
    await settle(tester);

    await tester.tapAt(tester.getTopLeft(find.byType(BlitzBody)) + const Offset(20, 40));
    await settle(tester);
    expect(opened, ['https://example.com/a']);

    await tester.tapAt(tester.getTopLeft(find.byType(BlitzBody)) + const Offset(20, 400));
    await settle(tester);
    expect(opened, hasLength(1));

    final start = tester.getTopLeft(find.byType(BlitzBody)) + const Offset(10, 200);
    final mouse = await tester.startGesture(start, kind: PointerDeviceKind.mouse);
    await mouse.moveBy(const Offset(60, 0));
    await mouse.moveBy(const Offset(60, 20));
    await mouse.up();
    await settle(tester);
    expect(page.selections.last, [10, 200, 130, 220]);

    await tester.sendKeyDownEvent(LogicalKeyboardKey.metaLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyC);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.metaLeft);
    await settle(tester);
    expect(copied, ['Alpha beta']);
  });

  testWidgets('late content lays the page out again; leaving closes it', (tester) async {
    await tester.pumpWidget(app());
    await settle(tester);
    final drawn = page.tops.length;
    page.changes.add(1);
    await settle(tester);
    expect(page.widths, hasLength(2));
    expect(page.widths.toSet(), hasLength(1));
    expect(page.tops.length, greaterThan(drawn));

    await tester.pumpWidget(app(show: false));
    expect(page.closed, isTrue);
  });
}
