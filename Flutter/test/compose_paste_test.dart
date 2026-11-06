import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/ui/compose.dart';

void main() {
  for (final hasFiles in [true, false]) {
    testWidgets(hasFiles ? 'file paste attaches while the body editor has focus' : 'text paste still replaces the body selection', (
      tester,
    ) async {
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
        if (call.method == 'Clipboard.getData') return {'text': 'pasted text'};
        return null;
      });
      final controller = TextEditingController(text: 'before selected after');
      final focus = FocusNode();
      addTearDown(controller.dispose);
      addTearDown(focus.dispose);
      var attachmentPastes = 0;
      await tester.pumpWidget(
        Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextEditingShortcuts(
            child: Actions(
              actions: {
                PasteTextIntent: AttachmentPasteAction(() async {
                  attachmentPastes++;
                  return hasFiles;
                }),
              },
              child: EditableText(
                controller: controller,
                focusNode: focus,
                style: const TextStyle(),
                cursorColor: const Color(0xff000000),
                backgroundCursorColor: const Color(0xffffffff),
              ),
            ),
          ),
        ),
      );
      focus.requestFocus();
      await tester.pump();
      controller.selection = const TextSelection(baseOffset: 7, extentOffset: 15);
      await tester.sendKeyDownEvent(LogicalKeyboardKey.metaLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyV);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.metaLeft);
      await tester.pump();
      expect(attachmentPastes, 1);
      expect(controller.text, hasFiles ? 'before selected after' : 'before pasted text after');
      await tester.pumpWidget(const SizedBox.shrink());
    }, variant: TargetPlatformVariant.only(TargetPlatform.macOS));
  }
}
