import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/controls.dart';
import 'package:mailstream_ui/ui/design.dart';

/// The app's own controls (MS-193): they work the same on the Mac and on Windows, in light and dark.
void main() {
  tearDown(() => Host.debugMacOverride = null);

  Widget host(Widget child, {Brightness brightness = Brightness.light}) => MacosApp(
    theme: MailTheme.macos(brightness),
    home: Center(child: child),
  );

  for (final mac in [true, false]) {
    for (final brightness in Brightness.values) {
      testWidgets('every control draws ${mac ? 'on the Mac' : 'on Windows'} in ${brightness.name}', (tester) async {
        Host.debugMacOverride = mac;
        await tester.pumpWidget(host(
          brightness: brightness,
          Column(mainAxisSize: MainAxisSize.min, children: [
            for (final kind in MailButtonKind.values) MailButton(label: kind.name, kind: kind, onPressed: () {}),
            MailButton(label: 'Busy', busy: true, onPressed: () {}),
            MailIconButton(icon: CupertinoIcons.search, tooltip: 'Icon', onPressed: () {}),
            MailSegmented<int>(selected: 1, onChanged: (_) {}, segments: const [MailSegment(0, 'A'), MailSegment(1, 'B')]),
            SizedBox(width: 200, child: MailTextField(controller: TextEditingController(), placeholder: 'Field')),
            SizedBox(width: 200, child: MailSearchField(controller: TextEditingController())),
            MailMenuButton<int>(value: 0, onChanged: (_) {}, items: const [MailMenuItem(0, 'Zero')]),
            MailDateButton(value: DateTime(2026, 11, 10), onChanged: (_) {}),
            MailSwitch(value: true, onChanged: (_) {}),
            MailCheckbox(value: false, onChanged: (_) {}),
          ]),
        ));
        await tester.pump(const Duration(seconds: 1));
        expect(tester.takeException(), isNull);
        expect(find.byType(AppKitView), findsNothing);
      });
    }
  }

  testWidgets('a button presses by click and by Enter, and not while disabled or busy', (tester) async {
    var pressed = 0;
    await tester.pumpWidget(host(Column(mainAxisSize: MainAxisSize.min, children: [
      MailButton(label: 'Go', autofocus: true, onPressed: () => pressed++),
      const MailButton(label: 'Off', onPressed: null),
      MailButton(label: 'Busy', busy: true, onPressed: () => pressed++),
    ])));
    await tester.tap(find.text('Go'));
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.tap(find.text('Off'));
    await tester.tap(find.text('Busy'));
    expect(pressed, 2);
  });

  testWidgets('a segmented control slides its card to the segment picked', (tester) async {
    var selected = 'day';
    await tester.pumpWidget(host(StatefulBuilder(
      builder: (context, setState) => MailSegmented<String>(
        selected: selected,
        onChanged: (value) => setState(() => selected = value),
        segments: const [MailSegment('agenda', 'Agenda'), MailSegment('day', 'Day'), MailSegment('week', 'Week')],
      ),
    )));
    AnimatedAlign indicator() => tester.widget<AnimatedAlign>(find.byKey(const ValueKey('segmentIndicator')));
    expect(indicator().alignment, const Alignment(0, 0));
    await tester.tap(find.text('Week'));
    await tester.pumpAndSettle();
    expect(selected, 'week');
    expect(indicator().alignment, const Alignment(1, 0));
  });

  testWidgets('a menu button opens its items, checks the chosen one and picks another', (tester) async {
    var value = 'b';
    await tester.pumpWidget(host(StatefulBuilder(
      builder: (context, setState) => MailMenuButton<String>(
        value: value,
        onChanged: (picked) => setState(() => value = picked),
        items: const [MailMenuItem('a', 'Alpha'), MailMenuItem('b', 'Beta'), MailMenuItem('c', 'Gamma')],
      ),
    )));
    expect(find.text('Beta'), findsOneWidget);
    expect(find.text('Alpha'), findsNothing);
    await tester.tap(find.text('Beta'));
    await tester.pumpAndSettle();
    expect(find.text('Alpha'), findsOneWidget);
    await tester.tap(find.text('Gamma'));
    await tester.pumpAndSettle();
    expect(value, 'c');
    expect(find.text('Alpha'), findsNothing, reason: 'the menu closed');
  });

  testWidgets('a long menu opens scrolled to the chosen item', (tester) async {
    await tester.pumpWidget(host(MailMenuButton<int>(
      value: 36,
      onChanged: (_) {},
      items: [for (var i = 0; i < 96; i++) MailMenuItem(i, 'Item $i')],
    )));
    await tester.tap(find.text('Item 36'));
    await tester.pumpAndSettle();
    expect(find.text('Item 36'), findsNWidgets(2), reason: 'the button and its row in the menu');
    final scroll = tester.widget<SingleChildScrollView>(find.byType(SingleChildScrollView)).controller!;
    expect(scroll.offset, 32 * 26, reason: 'item 36 four rows from the top');
  });

  testWidgets('a date button picks a day in its month and the next, keeping the time', (tester) async {
    var value = DateTime(2026, 11, 10, 14, 30);
    await tester.pumpWidget(host(StatefulBuilder(
      builder: (context, setState) => MailDateButton(value: value, onChanged: (day) => setState(() => value = day)),
    )));
    expect(find.text('Tue, Nov 10, 2026'), findsOneWidget);
    await tester.tap(find.text('Tue, Nov 10, 2026'));
    await tester.pumpAndSettle();
    expect(find.text('November 2026'), findsOneWidget);
    await tester.tap(find.text('24'));
    await tester.pumpAndSettle();
    expect(value, DateTime(2026, 11, 24, 14, 30));

    await tester.tap(find.text('Tue, Nov 24, 2026'));
    await tester.pumpAndSettle();
    await tester.tap(find.byIcon(CupertinoIcons.chevron_right));
    await tester.pumpAndSettle();
    expect(find.text('December 2026'), findsOneWidget);
    await tester.tap(find.text('31'));
    await tester.pumpAndSettle();
    expect(value, DateTime(2026, 12, 31, 14, 30));
  });

  testWidgets('a switch and a checkbox flip on a tap, and not while disabled', (tester) async {
    var on = false;
    var checked = false;
    await tester.pumpWidget(host(StatefulBuilder(
      builder: (context, setState) => Column(mainAxisSize: MainAxisSize.min, children: [
        MailSwitch(value: on, semanticLabel: 'Switch', onChanged: (value) => setState(() => on = value)),
        MailCheckbox(value: checked, semanticLabel: 'Check', onChanged: (value) => setState(() => checked = value)),
        const MailSwitch(value: false, semanticLabel: 'Off', onChanged: null),
      ]),
    )));
    await tester.tap(find.bySemanticsLabel('Switch'));
    await tester.tap(find.bySemanticsLabel('Check'));
    await tester.tap(find.bySemanticsLabel('Off'));
    await tester.pumpAndSettle();
    expect((on, checked), (true, true));
  });

  testWidgets('a confirmation is a sheet: Cancel answers no, the action yes', (tester) async {
    bool? answer;
    await tester.pumpWidget(host(Builder(
      builder: (context) => MailButton(
        label: 'Ask',
        onPressed: () async => answer = await confirmAction(context, title: 'Delete it?', message: 'Gone for good.', confirm: 'Delete'),
      ),
    )));
    await tester.tap(find.text('Ask'));
    await tester.pumpAndSettle();
    expect(find.text('Delete it?'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(answer, isFalse);
    await tester.tap(find.text('Ask'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete'));
    await tester.pumpAndSettle();
    expect(answer, isTrue);
  });
}
