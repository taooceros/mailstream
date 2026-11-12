import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/backend/models.dart';
import 'package:mailstream_ui/ui/common.dart';
import 'package:mailstream_ui/ui/design.dart';

/// Avatars (MS-192): the web reader's look, a person's initial in their system hue on a 15% wash of
/// it; the initial and the hue's slot are the engine's (MS-235), picked as the native app's
/// `Avatar.color` picks them.
void main() {
  Widget host(Widget child, Brightness brightness) => MacosApp(
    theme: MailTheme.macos(brightness),
    home: Center(child: child),
  );

  BoxDecoration face(WidgetTester tester) =>
      tester.widget<Container>(find.descendant(of: find.byType(PersonAvatar), matching: find.byType(Container)).first).decoration!
          as BoxDecoration;

  for (final brightness in Brightness.values) {
    final colors = brightness == Brightness.light ? MailColors.light : MailColors.dark;
    // FNV-1a 64 of "sender0@example.com" is 6 modulo 11: the system teal, as in the native app.
    final teal = colors.avatarPalette[6];

    testWidgets('an initial in its hue on a 15% wash in ${brightness.name}', (tester) async {
      await tester.pumpWidget(host(const PersonAvatar(address: Address(name: 'Sender 0', email: 'Sender0@Example.com')), brightness));
      expect(face(tester).color, teal.withValues(alpha: AvatarMetrics.tint));
      final letter = tester.widget<Text>(find.text('S'));
      expect((letter.style!.color, letter.style!.fontSize), (teal, ListMetrics.avatarSize * AvatarMetrics.letter));
    });

    testWidgets('ringed avatars lay their wash on the ring, opaque, in ${brightness.name}', (tester) async {
      await tester.pumpWidget(host(PersonAvatar(address: const Address(email: 'sender0@example.com'), ring: colors.card), brightness));
      final decoration = face(tester);
      expect(decoration.color, Color.alphaBlend(teal.withValues(alpha: AvatarMetrics.tint), colors.card));
      expect(decoration.color!.a, 1);
      expect((decoration.border! as Border).top.color, colors.card);
    });
  }

  testWidgets('a group without a picture shows its glyph in its hue', (tester) async {
    await tester.pumpWidget(host(
      const PersonAvatar(address: Address(name: 'Project 2', email: 'spaces/AAA'), icon: CupertinoIcons.person_3_fill),
      Brightness.light,
    ));
    final glyph = tester.widget<MacosIcon>(find.byType(MacosIcon));
    expect(glyph.color, MailColors.light.avatarHue(engine.avatar(name: 'Project 2', key: 'spaces/AAA').slot));
    expect(find.text('P'), findsNothing);
  });
}
