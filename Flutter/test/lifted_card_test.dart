import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/state/calendar_state.dart';
import 'package:mailstream_ui/ui/calendar_grid.dart';
import 'package:mailstream_ui/ui/common.dart';
import 'package:mailstream_ui/ui/design.dart';

import 'calendar_grid_test.dart' show meeting;

/// The decorations painted in the tree (a [Container] paints its own through a [DecoratedBox]).
Iterable<Decoration> _decorations(WidgetTester tester) =>
    tester.widgetList<DecoratedBox>(find.byType(DecoratedBox)).map((box) => box.decoration);

void main() {
  for (final brightness in Brightness.values) {
    final colors = brightness == Brightness.light ? MailColors.light : MailColors.dark;

    testWidgets('a list lifts only the row under the pointer, half as far as the selected one ($brightness)', (tester) async {
      await tester.pumpWidget(
        MacosTheme(
          data: MailTheme.macos(brightness),
          child: Directionality(
            textDirection: TextDirection.ltr,
            child: Column(children: [
              for (var i = 0; i < 3; i++)
                SizedBox(
                  key: ValueKey('row $i'),
                  height: ListMetrics.rowHeight,
                  child: LiftedRow(selected: i == 0, onTap: () {}, child: Text('Row $i')),
                ),
            ]),
          ),
        ),
      );
      List<bool> lifted() => [
        for (var i = 0; i < 3; i++)
          ...tester
              .widgetList<LiftedCard>(find.descendant(of: find.byKey(ValueKey('row $i')), matching: find.byType(LiftedCard)))
              .map((card) => card.selected),
      ];
      expect(lifted(), [true]);

      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      addTearDown(mouse.removePointer);
      await mouse.addPointer(location: tester.getCenter(find.byKey(const ValueKey('row 1'))));
      await tester.pump();
      expect(lifted(), [true, false]);
      expect(_decorations(tester), contains(LiftedCard.half(colors, const BorderRadius.all(Radius.circular(Radii.card)))));
      expect(_decorations(tester), contains(LiftedCard.lifted(colors, const BorderRadius.all(Radius.circular(Radii.card)))));

      // On to the next row: the one left behind settles back on the tray.
      await mouse.moveTo(tester.getCenter(find.byKey(const ValueKey('row 2'))));
      await tester.pump();
      final half = find.byWidgetPredicate((widget) => widget is LiftedCard && !widget.selected);
      expect(half, findsOneWidget);
      expect(find.ancestor(of: half, matching: find.byKey(const ValueKey('row 2'))), findsOneWidget);
    });

    testWidgets("the calendar's events lift as the lists' rows do, and half under the pointer ($brightness)", (tester) async {
      tester.view.physicalSize = const Size(1000, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final ahead = DateTime(2031, 3, 4);
      final review = meeting('Review', DateTime(2031, 3, 4, 9), 60, 'accepted');
      final sync = meeting('Sync', DateTime(2031, 3, 4, 11), 60, 'accepted');
      await tester.pumpWidget(MacosApp(
        theme: MailTheme.macos(brightness),
        home: CalendarGrid(
          days: [ahead],
          events: [review, sync],
          colorOf: (_) => const Color(0xFF4285F4),
          selected: eventKey(review),
          onOpen: (_) {},
        ),
      ));
      await tester.pumpAndSettle();
      const radius = BorderRadius.all(Radius.circular(CalendarMetrics.cardRadius));
      final lifted = LiftedCard.lifted(colors, radius);
      final half = LiftedCard.half(colors, radius, face: colors.card);
      expect(_decorations(tester).where((d) => d == lifted), hasLength(1));
      expect(_decorations(tester), isNot(contains(half)));

      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      addTearDown(mouse.removePointer);
      await mouse.addPointer(location: tester.getCenter(find.text('Sync')));
      await tester.pump();
      expect(_decorations(tester).where((d) => d == half), hasLength(1));
      expect(_decorations(tester).where((d) => d == lifted), hasLength(1));

      await mouse.moveTo(Offset.zero);
      await tester.pump();
      expect(_decorations(tester), isNot(contains(half)));
      await tester.pumpWidget(const SizedBox());
    });
  }
}
