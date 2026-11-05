import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/ui/slide_layer.dart';

void main() {
  Future<int> swipe(WidgetTester tester, double dx) async {
    var dismissed = 0;
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    await tester.pumpWidget(
      MaterialApp(
        home: SplitDetail(
          list: const SizedBox.expand(key: Key('list')),
          listWidth: 400,
          onDismiss: () => dismissed++,
          detail: const ColoredBox(key: Key('detail'), color: Colors.white),
        ),
      ),
    );
    final pointer = TestPointer(1, PointerDeviceKind.trackpad);
    final at = tester.getCenter(find.byKey(const Key('detail')));
    await tester.sendEventToBinding(pointer.panZoomStart(at));
    for (var i = 1; i <= 10; i++) {
      await tester.sendEventToBinding(pointer.panZoomUpdate(at, pan: Offset(dx * i / 10, 0)));
      await tester.pump(const Duration(milliseconds: 16));
    }
    // Mid-swipe the page follows the fingers and the list widens behind it.
    expect(tester.getSize(find.byKey(const Key('list'))).width, greaterThan(400));
    await tester.sendEventToBinding(pointer.panZoomEnd());
    await tester.pumpAndSettle();
    return dismissed;
  }

  testWidgets('a swipe past a third closes the split', (tester) async {
    expect(await swipe(tester, 500), 1);
    expect(tester.getSize(find.byKey(const Key('list'))).width, moreOrLessEquals(1200, epsilon: 0.5));
  });

  testWidgets('a short slow swipe springs back', (tester) async {
    expect(await swipe(tester, 60), 0);
    expect(tester.getSize(find.byKey(const Key('list'))).width, moreOrLessEquals(400, epsilon: 0.5));
  });

  testWidgets('closing without a swipe moves at once and eases into the full width', (tester) async {
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    Widget split(bool open) => MaterialApp(
      home: SplitDetail(
        list: const SizedBox.expand(key: Key('list')),
        listWidth: 400,
        onDismiss: () {},
        detail: open ? const ColoredBox(key: Key('detail'), color: Colors.white) : null,
      ),
    );
    double list() => tester.getSize(find.byKey(const Key('list'))).width;
    await tester.pumpWidget(split(true));
    await tester.pumpWidget(split(false));
    await tester.pump(const Duration(milliseconds: 16));
    // The close button responds within the first frame instead of creeping off.
    expect(list(), greaterThan(420));
    final widths = <double>[];
    while (tester.binding.hasScheduledFrame) {
      await tester.pump(const Duration(milliseconds: 16));
      widths.add(list());
    }
    expect(widths.last, moreOrLessEquals(1200, epsilon: 0.5));
    // It slows into the edge: the last frame of motion moves far less than the fastest one.
    final steps = [for (var i = 1; i < widths.length; i++) widths[i] - widths[i - 1]];
    expect(steps.reduce((a, b) => a > b ? a : b), greaterThan(steps.where((s) => s > 0).last * 10));
    expect(find.byKey(const Key('detail')), findsNothing);
  });

  testWidgets('the narrow layer slides out at once when closed', (tester) async {
    await tester.binding.setSurfaceSize(const Size(800, 800));
    Widget layer(bool open) => MaterialApp(
      home: SlideLayer(
        onDismiss: () {},
        child: open ? const SizedBox.expand(key: Key('page')) : null,
      ),
    );
    await tester.pumpWidget(layer(true));
    await tester.pumpAndSettle();
    await tester.pumpWidget(layer(false));
    await tester.pump(const Duration(milliseconds: 16));
    expect(tester.getTopLeft(find.byKey(const Key('page'))).dx, greaterThan(20));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('page')), findsNothing);
  });
}
