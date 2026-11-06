import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/ui/reader.dart';

void main() {
  Widget view(EndPin pin, double height) => Directionality(
    textDirection: TextDirection.ltr,
    child: NotificationListener<Notification>(
      onNotification: pin.follow,
      child: SingleChildScrollView(
        controller: pin.controller,
        child: SizedBox(key: const Key('content'), height: height, width: 400),
      ),
    ),
  );

  Future<EndPin> open(WidgetTester tester) async {
    await tester.binding.setSurfaceSize(const Size(400, 600));
    final pin = EndPin(ScrollController());
    addTearDown(pin.controller.dispose);
    await tester.pumpWidget(view(pin, 2000));
    await tester.pump();
    return pin;
  }

  Future<void> grow(WidgetTester tester, EndPin pin) async {
    await tester.pumpWidget(view(pin, 3000));
    await tester.pump();
  }

  testWidgets('opens at the end and follows the content as it grows', (tester) async {
    final pin = await open(tester);
    expect(pin.controller.offset, 1400);
    await grow(tester, pin);
    expect(pin.controller.offset, 2400);
  });

  testWidgets('a drag lets go: growing content no longer moves the view', (tester) async {
    final pin = await open(tester);
    await tester.dragFrom(const Offset(200, 300), const Offset(0, 300));
    await tester.pumpAndSettle();
    final offset = pin.controller.offset;
    expect(offset, lessThan(1400));
    await grow(tester, pin);
    expect(pin.controller.offset, offset);
  });

  testWidgets('a mouse wheel lets go too', (tester) async {
    final pin = await open(tester);
    await tester.sendEventToBinding(const PointerScrollEvent(position: Offset(200, 300), scrollDelta: Offset(0, -200)));
    await tester.pumpAndSettle();
    expect(pin.controller.offset, 1200);
    await grow(tester, pin);
    expect(pin.controller.offset, 1200);
  });

  testWidgets('release lets go without scrolling', (tester) async {
    final pin = await open(tester);
    pin.release();
    await grow(tester, pin);
    expect(pin.controller.offset, 1400);
  });
}
