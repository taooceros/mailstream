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
          pageId: 'detail',
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
        pageId: open ? 'detail' : null,
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
        pageId: open ? 'page' : null,
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
        pageId: 'detail',
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
        pageId: 'detail',
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
          pageId: 'page',
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

  testWidgets('a completed split swipe removes its native detail after the owner closes it', (tester) async {
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    final open = ValueNotifier(true);
    addTearDown(open.dispose);
    await tester.pumpWidget(Directionality(
      textDirection: TextDirection.ltr,
      child: ValueListenableBuilder(
        valueListenable: open,
        builder: (context, shown, _) => SplitDetail(
          pageId: shown ? 'detail' : null,
          list: const SizedBox.expand(key: Key('list')),
          listWidth: 400,
          onDismiss: () => open.value = false,
          detail: shown ? const ColoredBox(key: Key('detail'), color: Color(0xFFFFFFFF)) : null,
        ),
      ),
    ));
    final at = tester.getCenter(find.byKey(const Key('detail')));
    final pointer = TestPointer(1, PointerDeviceKind.trackpad);
    await tester.sendEventToBinding(pointer.panZoomStart(at));
    await tester.sendEventToBinding(pointer.panZoomUpdate(at, pan: const Offset(500, 0)));
    await tester.pump();
    await tester.sendEventToBinding(pointer.panZoomEnd());
    await tester.pumpAndSettle();
    expect(open.value, isFalse);
    expect(tester.getSize(find.byKey(const Key('list'))).width, moreOrLessEquals(1200, epsilon: 0.5));
    expect(find.byKey(const Key('detail')), findsNothing);
  });

  for (final split in [false, true]) {
    testWidgets('${split ? 'split' : 'layer'} closing never removes a newly opened page', (tester) async {
      await tester.binding.setSurfaceSize(Size(split ? 1200 : 800, 800));
      final page = ValueNotifier<int?>(1);
      addTearDown(page.dispose);
      var dismissed = 0;
      void dismiss() {
        dismissed++;
        page.value = null;
      }
      await tester.pumpWidget(Directionality(
        textDirection: TextDirection.ltr,
        child: ValueListenableBuilder(
          valueListenable: page,
          builder: (context, id, _) {
            final child = id == null ? null : ColoredBox(key: ValueKey(id), color: const Color(0xFFFFFFFF));
            return split
                ? SplitDetail(pageId: id, list: const SizedBox.expand(), detail: child, listWidth: 400, onDismiss: dismiss)
                : SlideLayer(pageId: id, onDismiss: dismiss, child: child);
          },
        ),
      ));
      await tester.pumpAndSettle();
      Future<void> swipe(int id, int pointerId) async {
        final at = tester.getCenter(find.byKey(ValueKey(id)));
        final pointer = TestPointer(pointerId, PointerDeviceKind.trackpad);
        await tester.sendEventToBinding(pointer.panZoomStart(at));
        await tester.sendEventToBinding(pointer.panZoomUpdate(at, pan: const Offset(500, 0)));
        await tester.pump();
        await tester.sendEventToBinding(pointer.panZoomEnd());
        await tester.pump(const Duration(milliseconds: 40));
      }
      await swipe(1, 1);
      page.value = 2;
      await tester.pumpAndSettle();
      expect(dismissed, 0);
      expect(find.byKey(const ValueKey(1)), findsNothing);
      expect(find.byKey(const ValueKey(2)), findsOneWidget);
      expect(tester.getTopLeft(find.byKey(const ValueKey(2))).dx, moreOrLessEquals(split ? 408 : 0, epsilon: 0.5));

      page.value = null;
      await tester.pump(const Duration(milliseconds: 40));
      page.value = 3;
      await tester.pumpAndSettle();
      expect(find.byKey(const ValueKey(3)), findsOneWidget);
      expect(dismissed, 0);

      // A normal rebuild of the same page must not cancel its pending dismissal.
      await swipe(3, 2);
      page.notifyListeners();
      await tester.pumpAndSettle();
      expect(dismissed, 1);
      expect(page.value, isNull);
      expect(find.byKey(const ValueKey(3)), findsNothing);
    });
  }

  // MS-132: after a rest the first frames render slowly, so a swipe that starts while reading must
  // not be the first to draw.
  for (final split in [false, true]) {
    testWidgets('${split ? 'split' : 'layer'} keeps drawing while a page is shown and warms up as fingers land', (tester) async {
      await tester.binding.setSurfaceSize(Size(split ? 1200 : 800, 800));
      Widget build(bool open) {
        final page = open ? const ColoredBox(key: Key('page'), color: Color(0xFFFFFFFF)) : null;
        return Directionality(
          textDirection: TextDirection.ltr,
          child: split
              ? SplitDetail(pageId: open ? 'page' : null, list: const SizedBox.expand(), listWidth: 400, onDismiss: () {}, detail: page)
              : SlideLayer(pageId: open ? 'page' : null, onDismiss: () {}, child: page),
        );
      }

      await tester.pumpWidget(build(true));
      await tester.pumpAndSettle();
      expect(tester.binding.hasScheduledFrame, isFalse);
      // A frame comes before the embedder's one-second idle timeout drops its surfaces.
      await tester.binding.delayed(SwipeReadiness.keepAlive);
      expect(tester.binding.hasScheduledFrame, isTrue);
      await tester.pump();

      // Fingers resting on the page draw frames before they move it, for a while.
      final at = tester.getCenter(find.byKey(const Key('page')));
      final left = tester.getTopLeft(find.byKey(const Key('page'))).dx;
      final pointer = TestPointer(1, PointerDeviceKind.trackpad);
      await tester.sendEventToBinding(pointer.panZoomStart(at));
      expect(tester.binding.hasScheduledFrame, isTrue);
      var frames = 0;
      while (tester.binding.hasScheduledFrame && frames < 100) {
        await tester.pump(const Duration(milliseconds: 8));
        frames++;
      }
      expect(frames * 8, inInclusiveRange(SwipeReadiness.warmUp.inMilliseconds, SwipeReadiness.warmUp.inMilliseconds + 24));
      expect(tester.getTopLeft(find.byKey(const Key('page'))).dx, left);
      await tester.sendEventToBinding(pointer.panZoomEnd());

      // Closed, the window rests.
      await tester.pumpWidget(build(false));
      await tester.pumpAndSettle();
      await tester.binding.delayed(const Duration(seconds: 2));
      expect(tester.binding.hasScheduledFrame, isFalse);
    });
  }
}
