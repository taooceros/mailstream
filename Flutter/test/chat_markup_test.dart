import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/ui/chat_markup.dart';

/// Every run of text `ChatText` draws, with whether it is bold, italic or struck through.
Future<List<String>> draw(WidgetTester tester, String markdown) async {
  await tester.pumpWidget(
    MacosTheme(
      data: MacosThemeData.light(),
      child: Directionality(
        textDirection: TextDirection.ltr,
        child: ChatText(text: markdown, names: const {'users/1': 'Brent'}, onOpen: (_) {}),
      ),
    ),
  );
  final runs = <String>[];
  void walk(InlineSpan span, TextStyle inherited) {
    if (span is! TextSpan) return;
    final style = inherited.merge(span.style);
    if (span.text case final text?) {
      final marks = [
        if (style.fontWeight == FontWeight.w700) 'b',
        if (style.fontStyle == FontStyle.italic) 'i',
        if (style.decoration == TextDecoration.lineThrough) 's',
      ].join();
      runs.add(marks.isEmpty ? text : '$marks:$text');
    }
    for (final child in span.children ?? const <InlineSpan>[]) {
      walk(child, style);
    }
  }

  for (final text in tester.widgetList<RichText>(find.byType(RichText))) {
    walk(text.text, const TextStyle());
  }
  return runs;
}

void main() {
  testWidgets('Google Chat Markdown, as the API answers it', (tester) async {
    final runs = await draw(
      tester,
      '**Office:** CSE 592\n'
      'line two *it* ~~gone~~ \\*\\*literal\\*\\* 1\\. done\\!\n'
      '\n'
      '* item\n'
      '    * nested\n'
      '\n'
      '1. first\n'
      '2. second\n'
      '\n'
      '```\na\\*b\n```\n'
      'see [site](https://x.y) and <chat-user data-user="users/1"></chat-user> &amp; https://bare.example',
    );
    expect(runs, containsAllInOrder(['b:Office:', ' CSE 592\nline two ', 'i:it', 's:gone']));
    expect(runs.join(), contains('**literal** 1. done!'));
    expect(runs, containsAllInOrder(['•', 'item', '•', 'nested', '1.', 'first', '2.', 'second']));
    expect(runs, contains(r'a\*b'));
    expect(runs.join(), contains('see site and @Brent & https://bare.example'));
  });
}
