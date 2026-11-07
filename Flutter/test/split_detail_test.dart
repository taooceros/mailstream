import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/ui/slide_layer.dart';

void main() {
  Future<int> swipe(WidgetTester tester, double dx) async {
    var dismissed = 0;
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: SplitDetail(
          list: const SizedBox.expand(key: Key('list')),
          listWidth: 400,
          onDismiss: () => dismissed++,
          detail: const ColoredBox(key: Key('detail'), color: Color(0xFFFFFFFF)),
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
    Widget split(bool open) => Directionality(
      textDirection: TextDirection.ltr,
      child: SplitDetail(
        list: const SizedBox.expand(key: Key('list')),
        listWidth: 400,
        onDismiss: () {},
        detail: open ? const ColoredBox(key: Key('detail'), color: Color(0xFFFFFFFF)) : null,
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
    Widget layer(bool open) => Directionality(
      textDirection: TextDirection.ltr,
      child: SlideLayer(
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

  testWidgets('a new swipe picks up the split where its canceled swipe is settling', (tester) async {
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    await tester.pumpWidget(Directionality(
      textDirection: TextDirection.ltr,
      child: SplitDetail(
        list: const SizedBox.expand(),
        listWidth: 400,
        onDismiss: () {},
        detail: const ColoredBox(key: Key('detail'), color: Color(0xFFFFFFFF)),
      ),
    ));
    final at = tester.getCenter(find.byKey(const Key('detail')));
    final first = TestPointer(1, PointerDeviceKind.trackpad);
    await tester.sendEventToBinding(first.panZoomStart(at));
    await tester.sendEventToBinding(first.panZoomUpdate(at, pan: const Offset(160, 0)));
    await tester.pump();
    await tester.sendEventToBinding(first.panZoomEnd());
    await tester.pump(const Duration(milliseconds: 40));
    final before = tester.getTopLeft(find.byKey(const Key('detail'))).dx;
    final next = TestPointer(2, PointerDeviceKind.trackpad);
    await tester.sendEventToBinding(next.panZoomStart(at));
    await tester.sendEventToBinding(next.panZoomUpdate(at, pan: const Offset(8, 0)));
    await tester.pump();
    expect(tester.getTopLeft(find.byKey(const Key('detail'))).dx, moreOrLessEquals(before + 8, epsilon: 0.5));
    await tester.sendEventToBinding(next.panZoomUpdate(at, pan: const Offset(-8, 0)));
    await tester.pump();
    expect(tester.getTopLeft(find.byKey(const Key('detail'))).dx, moreOrLessEquals(before - 8, epsilon: 0.5));
    await tester.sendEventToBinding(next.panZoomEnd());
    await tester.pumpAndSettle();
  });

  testWidgets('a vertical gesture does not freeze the split spring', (tester) async {
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    await tester.pumpWidget(Directionality(
      textDirection: TextDirection.ltr,
      child: SplitDetail(
        list: const SizedBox.expand(),
        listWidth: 400,
        onDismiss: () {},
        detail: const ColoredBox(key: Key('detail'), color: Color(0xFFFFFFFF)),
      ),
    ));
    final at = tester.getCenter(find.byKey(const Key('detail')));
    final first = TestPointer(1, PointerDeviceKind.trackpad);
    await tester.sendEventToBinding(first.panZoomStart(at));
    await tester.sendEventToBinding(first.panZoomUpdate(at, pan: const Offset(160, 0)));
    await tester.pump();
    await tester.sendEventToBinding(first.panZoomEnd());
    await tester.pump(const Duration(milliseconds: 40));
    final before = tester.getTopLeft(find.byKey(const Key('detail'))).dx;
    final vertical = TestPointer(2, PointerDeviceKind.trackpad);
    await tester.sendEventToBinding(vertical.panZoomStart(at));
    await tester.sendEventToBinding(vertical.panZoomUpdate(at, pan: const Offset(0, 30)));
    await tester.pump(const Duration(milliseconds: 300));
    expect(tester.getTopLeft(find.byKey(const Key('detail'))).dx, lessThan(before - 20));
    await tester.sendEventToBinding(vertical.panZoomEnd());
    await tester.pumpAndSettle();
  });

  for (final vertical in [false, true]) {
    testWidgets('the narrow layer ${vertical ? 'keeps settling during vertical input' : 'continues an interrupted swipe without jumping'}', (tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 800));
      await tester.pumpWidget(Directionality(
        textDirection: TextDirection.ltr,
        child: SlideLayer(
          onDismiss: () {},
          child: const ColoredBox(key: Key('page'), color: Color(0xFFFFFFFF)),
        ),
      ));
      await tester.pumpAndSettle();
      final page = find.byKey(const Key('page'));
      final at = tester.getCenter(page);
      final first = TestPointer(1, PointerDeviceKind.trackpad);
      await tester.sendEventToBinding(first.panZoomStart(at));
      await tester.sendEventToBinding(first.panZoomUpdate(at, pan: const Offset(160, 0)));
      await tester.pump();
      await tester.sendEventToBinding(first.panZoomEnd());
      await tester.pump(const Duration(milliseconds: 40));
      final before = tester.getTopLeft(page).dx;
      final next = TestPointer(2, PointerDeviceKind.trackpad);
      await tester.sendEventToBinding(next.panZoomStart(at));
      await tester.sendEventToBinding(next.panZoomUpdate(at, pan: vertical ? const Offset(0, 30) : const Offset(8, 0)));
      await tester.pump(vertical ? const Duration(milliseconds: 300) : Duration.zero);
      expect(tester.getTopLeft(page).dx, vertical ? lessThan(before - 20) : moreOrLessEquals(before + 8, epsilon: 0.5));
      await tester.sendEventToBinding(next.panZoomEnd());
      await tester.pumpAndSettle();
    });
  }
}
