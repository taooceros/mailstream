import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/ui/common.dart';
import 'package:mailstream_ui/ui/design.dart';

/// Tags, marks and unread counts (MS-190): one tag, one 6 pt dot for accounts and calendars, and
/// counts that are never red.
void main() {
  Widget host(Widget child, {Brightness brightness = Brightness.light}) => MacosApp(
    theme: MailTheme.macos(brightness),
    home: Center(child: child),
  );

  for (final brightness in Brightness.values) {
    testWidgets('tags take the tag role on a 12% wash, in two heights, in ${brightness.name}', (tester) async {
      final colors = brightness == Brightness.light ? MailColors.light : MailColors.dark;
      await tester.pumpWidget(host(
        brightness: brightness,
        Column(mainAxisSize: MainAxisSize.min, children: [
          Tag(label: 'Work', color: colors.accent),
          const Tag(label: 'Going', compact: true),
          const Tag(icon: CupertinoIcons.video_camera, label: 'Meet', compact: true),
          // Squeezed by its neighbors: cut at its edge, without overflowing.
          const SizedBox(width: 4, child: Tag(icon: CupertinoIcons.video_camera, label: 'Meet', compact: true)),
        ]),
      ));
      expect(tester.takeException(), isNull);
      final tags = find.byType(Tag);
      expect(tester.getSize(tags.at(0)).height, TagMetrics.height);
      expect(tester.getSize(tags.at(1)).height, TagMetrics.compactHeight);
      final wash = tester.widget<Container>(find.descendant(of: tags.at(0), matching: find.byType(Container)).first);
      expect((wash.decoration! as ShapeDecoration).color, colors.accent.withValues(alpha: TagMetrics.tint));
      final work = tester.widget<Text>(find.text('Work'));
      expect((work.style!.fontSize, work.style!.fontWeight, work.style!.color), (10, FontWeight.w600, colors.accent));
      // Neutral without a color.
      final going = tester.widget<Text>(find.text('Going'));
      expect(going.style!.color, colors.secondaryLabel);
    });

    testWidgets('the rail\'s count is a neutral capsule in ${brightness.name}', (tester) async {
      final colors = brightness == Brightness.light ? MailColors.light : MailColors.dark;
      await tester.pumpWidget(host(brightness: brightness, const CountBadge(count: 12, child: SizedBox.square(dimension: 19))));
      final capsule = tester.widget<Container>(find.ancestor(of: find.text('12'), matching: find.byType(Container)).first);
      final fill = (capsule.decoration! as ShapeDecoration).color;
      expect(fill, colors.strongFill);
      expect(fill, isNot(colors.red));
    });
  }

  testWidgets('an account chip carries a 6 pt dot, not a bar', (tester) async {
    await tester.pumpWidget(host(MarkChip(label: 'bench0@gmail.com', mark: accountColor(0))));
    final dot = find.byType(MarkDot);
    expect(dot, findsOneWidget);
    expect(tester.getSize(dot), const Size.square(MarkMetrics.dot));
  });

  testWidgets('unread is a dot in its column, nothing when read', (tester) async {
    await tester.pumpWidget(host(const Row(mainAxisSize: MainAxisSize.min, children: [UnreadDot(unread: true), UnreadDot(unread: false)])));
    final slots = find.byType(UnreadDot);
    expect(tester.getSize(slots.at(0)).width, MarkMetrics.unreadSlot);
    expect(find.descendant(of: slots.at(0), matching: find.byType(DecoratedBox)), findsOneWidget);
    expect(find.descendant(of: slots.at(1), matching: find.byType(DecoratedBox)), findsNothing);
  });
}
