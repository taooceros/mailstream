import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream_ui/ui/chat_markup.dart';

String plain(String block) {
  final spans = ChatMarkup.inline(
    block,
    code: const TextStyle(),
    link: const TextStyle(),
    mention: const TextStyle(),
    names: {'users/1': 'Brent'},
    open: (_) => TapGestureRecognizer(),
  );
  return TextSpan(children: spans).toPlainText();
}

void main() {
  test('escapes, emphasis, lists, code', () {
    final blocks = ChatMarkup.blocks(
      'Title: *PhaseMorph* \\*\\*not bold\\*\\* 1\\. done\\!\n* item _one_\n    * nested\n```\na\\*b\n```\nsee <https://x.y|site> <users/1>',
    );
    final texts = [
      for (final b in blocks)
        switch (b) {
          ChatParagraph(:final text) => 'P:${plain(text)}',
          ChatBullet(:final text, :final depth) => 'B$depth:${plain(text)}',
          ChatCode(:final text) => 'C:$text',
          ChatQuote(:final text) => 'Q:${plain(text)}',
        },
    ];
    expect(texts, [
      'P:Title: PhaseMorph **not bold** 1. done!',
      'B0:item one',
      'B1:nested',
      r'C:a\*b',
      'P:see site @Brent',
    ]);
  });
}
