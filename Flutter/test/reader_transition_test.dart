import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/reader.dart';

import 'reader_fixture.dart';

void main() {
  testWidgets(
    'conversation fade keeps native reader geometry unscaled throughout switching',
    (tester) async {
      final mail = ReaderMail();
      final state = MailState(mail);
      await state.start();
      addTearDown(state.dispose);
      await tester.binding.setSurfaceSize(const Size(1240, 800));
      addTearDown(() => tester.binding.setSurfaceSize(null));
      await tester.pumpWidget(MailstreamApp(state: state));
      state.open(state.conversations.first);
      mail.answer(0, mail.rows.first);
      await tester.pumpAndSettle();

      void unscaled() {
        final readers = find.byType(Reader);
        expect(readers, findsWidgets);
        for (final element in readers.evaluate()) {
          element.visitAncestorElements((ancestor) {
            expect(
              ancestor.widget,
              isNot(isA<ScaleTransition>()),
              reason: 'WebKit clips in native viewport coordinates; the conversation fade must not scale it',
            );
            return true;
          });
        }
      }

      unscaled();
      state.open(state.conversations.last);
      mail.answer(1, mail.rows.last);
      await tester.pump();
      // Include the outgoing and incoming reader while both coexist in AnimatedSwitcher.
      for (var frame = 0; frame < 16; frame++) {
        await tester.pump(const Duration(milliseconds: 16));
        unscaled();
      }
      await tester.pumpAndSettle();
      unscaled();
      expect(
        find.textContaining('Selected conversation B stays open.'),
        findsOneWidget,
      );
      await tester.pumpWidget(const SizedBox());
    },
  );

  testWidgets('reader stays top-aligned while delayed thread content expands', (tester) async {
    final mail = ReaderMail();
    final state = MailState(mail);
    await state.start();
    addTearDown(state.dispose);
    await tester.binding.setSurfaceSize(const Size(1240, 800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(MailstreamApp(state: state));
    state.open(state.conversations.first);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));
    final before = tester.getRect(find.byType(Reader));

    mail.answer(0, conversation('a', count: 30));
    await tester.pumpAndSettle();
    final after = tester.getRect(find.byType(Reader));
    expect(before.top, moreOrLessEquals(after.top, epsilon: 0.5));
    expect(before.size, after.size);
    await tester.pumpWidget(const SizedBox());
  });
}
