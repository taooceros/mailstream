import 'package:flutter/cupertino.dart' show CupertinoIcons;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/ui/controls.dart';
import 'package:mailstream_ui/ui/design.dart';

/// Notices and attachment colors (MS-191): one notice in two densities on a quiet wash of its
/// tone, and attachment glyphs in the app's system hues.
void main() {
  Widget host(Widget child, {Brightness brightness = Brightness.light}) => MacosApp(
    theme: MailTheme.macos(brightness),
    home: Center(child: SizedBox(width: 480, child: child)),
  );

  BoxDecoration face(WidgetTester tester, Finder notice) =>
      tester.widget<AnimatedContainer>(find.descendant(of: notice, matching: find.byType(AnimatedContainer)).first).decoration!
          as BoxDecoration;

  for (final brightness in Brightness.values) {
    testWidgets('a card and a line share one look, on a quieter wash in dark mode, in ${brightness.name}', (tester) async {
      final colors = brightness == Brightness.light ? MailColors.light : MailColors.dark;
      final tint = brightness == Brightness.light ? NoticeMetrics.tint : NoticeMetrics.darkTint;
      var opened = 0;
      await tester.pumpWidget(host(
        brightness: brightness,
        Column(mainAxisSize: MainAxisSize.min, children: [
          MailNotice(
            tone: MailNoticeTone.action,
            icon: CupertinoIcons.calendar_badge_plus,
            title: 'bench1@gmail.com',
            message: 'Sign in again to allow calendar access.',
            trailing: MailButton(label: 'Sign In…', onPressed: () {}),
          ),
          MailNotice(compact: true, tone: MailNoticeTone.error, message: '2 accounts cannot show their calendars', action: 'Show in Agenda', onTap: () => opened++),
          const MailNotice(tone: MailNoticeTone.error, message: 'AI tagging failed'),
        ]),
      ));
      expect(tester.takeException(), isNull);
      final notices = find.byType(MailNotice);
      final card = face(tester, notices.at(0));
      expect(card.color, colors.waiting.withValues(alpha: tint));
      expect(card.borderRadius, BorderRadius.circular(Radii.card));
      expect(face(tester, notices.at(2)).color, colors.red.withValues(alpha: tint));
      expect(tester.getSize(notices.at(1)).height, NoticeMetrics.compactHeight);
      expect(find.text('Sign In…'), findsOneWidget);
      // Errors take the warning glyph unless told otherwise.
      expect(find.descendant(of: notices.at(2), matching: find.byIcon(CupertinoIcons.exclamationmark_triangle)), findsOneWidget);
      final action = tester.widget<Text>(find.text('Show in Agenda'));
      expect(action.style!.color, colors.onRedFill);
      await tester.tap(notices.at(1));
      expect(opened, 1);
    });
  }

  test('every attachment kind has its own color, in both modes', () {
    for (final brightness in Brightness.values) {
      final hues = {for (final kind in AttachmentKind.values) attachmentColor(kind, brightness)};
      expect(hues.length, AttachmentKind.values.length);
    }
    expect(attachmentColor(AttachmentKind.pdf, Brightness.light), MailColors.light.red);
    expect(attachmentColor(AttachmentKind.document, Brightness.dark), MailColors.dark.accent);
  });
}
