import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/sidebar.dart';
import 'package:mailstream_ui/ui/slide_layer.dart';

import 'column_width_app.dart' show ColumnWidthMail;

// MS-185: the list beside an open page takes a share of the window, and the gap between them
// drags it, within limits, and double-clicks back to the default.
void main() {
  test('a list fits between its minimum and the narrowest page beside it', () {
    expect(SplitDetail.fit(480, 1200), 480);
    expect(SplitDetail.fit(100, 1200), SplitDetail.minList);
    expect(SplitDetail.fit(1000, 1200), 1200 - Panes.gap - SplitDetail.minDetail);
    // Too narrow for both: the list keeps its minimum and the page gives way.
    expect(SplitDetail.fit(500, 600), SplitDetail.minList);
  });

  double listWidth(WidgetTester tester) =>
      tester.getSize(find.descendant(of: find.byType(SplitDetail), matching: find.byKey(const Key('list')))).width;

  Future<(List<double>, List<int>)> split(WidgetTester tester, {double width = 480}) async {
    final kept = <double>[];
    final resets = <int>[];
    await tester.binding.setSurfaceSize(const Size(1200, 800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: SplitDetail(
          pageId: 'detail',
          list: const SizedBox.expand(key: Key('list')),
          listWidth: width,
          onDismiss: () {},
          onResize: kept.add,
          onResetWidth: () => resets.add(1),
          detail: const ColoredBox(key: Key('detail'), color: Color(0xFFFFFFFF)),
        ),
      ),
    );
    return (kept, resets);
  }

  testWidgets('dragging the gap resizes the list and hands back its width', (tester) async {
    final (kept, _) = await split(tester);
    expect(listWidth(tester), 480);
    final gap = find.byType(ResizeGap);
    expect(gap, findsOneWidget);

    final drag = await tester.startGesture(tester.getCenter(gap), kind: PointerDeviceKind.mouse);
    await drag.moveBy(const Offset(20, 0));
    await drag.moveBy(const Offset(100, 0));
    await tester.pump();
    // Mid-drag the list follows the pointer; nothing is kept yet.
    expect(listWidth(tester), closeTo(600, 0.01));
    expect(tester.getTopLeft(find.byKey(const Key('detail'))).dx, closeTo(600 + Panes.gap, 0.01));
    expect(kept, isEmpty);

    // Past the limit it stops, and coming back resumes under the pointer.
    await drag.moveBy(const Offset(400, 0));
    await tester.pump();
    expect(listWidth(tester), 1200 - Panes.gap - SplitDetail.minDetail);
    await drag.moveBy(const Offset(-400, 0));
    await tester.pump();
    expect(listWidth(tester), closeTo(600, 0.01));
    await drag.up();
    await tester.pump(kDoubleTapTimeout);
    await tester.pump();
    expect(kept.single, closeTo(600, 0.01));
    expect(listWidth(tester), closeTo(600, 0.01));
  });

  testWidgets('the list keeps the dragged width until its owner passes one back', (tester) async {
    final (kept, _) = await split(tester);
    final drag = await tester.startGesture(tester.getCenter(find.byType(ResizeGap)), kind: PointerDeviceKind.mouse);
    await drag.moveBy(const Offset(20, 0));
    await drag.moveBy(const Offset(-200, 0));
    await drag.up();
    await tester.pump(kDoubleTapTimeout);
    await tester.pump();
    expect(listWidth(tester), closeTo(SplitDetail.minList, 0.01));
    expect(kept.single, closeTo(SplitDetail.minList, 0.01));
    // The owner's rounded share of it rules once it arrives.
    await split(tester, width: 301);
    expect(listWidth(tester), 301);
  });

  testWidgets('a double-click on the gap resets the width', (tester) async {
    final (_, resets) = await split(tester);
    final at = tester.getCenter(find.byType(ResizeGap));
    await tester.tapAt(at, kind: PointerDeviceKind.mouse);
    await tester.pump(const Duration(milliseconds: 50));
    await tester.tapAt(at, kind: PointerDeviceKind.mouse);
    await tester.pumpAndSettle();
    expect(resets, [1]);
  });

  testWidgets('the gap does not drag without an owner keeping the width', (tester) async {
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: SplitDetail(
          pageId: 'detail',
          list: const SizedBox.expand(key: Key('list')),
          listWidth: 400,
          onDismiss: () {},
          detail: const SizedBox.expand(),
        ),
      ),
    );
    expect(find.byType(ResizeGap), findsNothing);
  });

  test('a column takes its default share until one is kept, and resets to it', () async {
    final mail = ColumnWidthMail();
    final state = MailState(mail);
    expect(state.columnShare(MailState.listWidthSetting), 0.4);
    expect(state.columnShare(MailState.calendarWidthSetting), 0.5);
    await state.reloadSettings();
    await state.setColumnShare(MailState.calendarWidthSetting, 0.6234);
    expect(mail.widths, {MailState.calendarWidthSetting: 623});
    expect(state.columnShare(MailState.calendarWidthSetting), 0.623);
    expect(state.columnShare(MailState.listWidthSetting), 0.4);
    await state.setColumnShare(MailState.calendarWidthSetting, null);
    expect(state.columnShare(MailState.calendarWidthSetting), 0.5);
    state.dispose();
  });

  // The shell's list beside an open conversation is the same share of the window at any size.
  for (final size in const [Size(1300, 820), Size(2200, 1300)]) {
    testWidgets('the list takes 40% of a ${size.width.round()} wide window beside the sidebar', (tester) async {
      final mail = ColumnWidthMail();
      final state = MailState(mail);
      await state.start();
      addTearDown(state.dispose);
      await tester.binding.setSurfaceSize(size);
      addTearDown(() => tester.binding.setSurfaceSize(null));
      await tester.pumpWidget(MailstreamApp(state: state));
      state.open(state.conversations.first);
      mail.answer(0, mail.rows.first);
      await tester.pumpAndSettle();

      final content = size.width - Sidebar.width - Panes.gap;
      final split = find.byType(SplitDetail);
      final list = tester.getSize(find.descendant(of: split, matching: find.byType(Positioned)).first).width;
      expect(list, closeTo(content * 0.4, 0.5));
      await tester.pumpWidget(const SizedBox());
    });
  }
}
