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
}
