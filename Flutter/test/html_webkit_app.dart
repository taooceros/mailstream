import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/state/mail_state.dart';
import 'package:mailstream_ui/ui/mail_resources.dart';

import 'reader_fixture.dart';

// The WebKit body on mail as the engine hands it over (MS-167): quoted history folding in the
// page, inline images from the inline cache (a 1x picture and its 2x `srcset`), and a forward
// that is nothing but quote. Synthetic mail only: no real backend, accounts or private mail; the
// pictures are drawn at launch into the inline cache's synthetic folder `f167…`.
// MAILSTREAM_READER_RENDERER=webKit flutter run -d macos -t test/html_webkit_app.dart
void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  await _drawInlineParts();
  final state = MailState(WebKitMail());
  await state.start();
  runApp(MailstreamApp(state: state));
}

const _key = 'f167f167f167f167f167f167f167f167';
String get _folder => '$inlineCacheRoot/$_key';

/// The engine's address of an inline part, as it hands HTML to Flutter.
String _part(String name) => '$inlineCacheUrl$_key/$name';

/// A 160×48 point badge saying [label], at [scale] pixels per point.
Future<void> _badge(String name, String label, double scale, ui.Color color) async {
  final recorder = ui.PictureRecorder();
  final canvas = ui.Canvas(recorder)..scale(scale);
  canvas.drawRRect(ui.RRect.fromLTRBR(0, 0, 160, 48, const ui.Radius.circular(10)), ui.Paint()..color = color);
  final text = (ui.ParagraphBuilder(ui.ParagraphStyle(textAlign: ui.TextAlign.center, fontSize: 15))
        ..pushStyle(ui.TextStyle(color: const ui.Color(0xffffffff)))
        ..addText(label))
      .build()
    ..layout(const ui.ParagraphConstraints(width: 160));
  canvas.drawParagraph(text, const ui.Offset(0, 14));
  final image = await recorder.endRecording().toImage((160 * scale).round(), (48 * scale).round());
  final png = await image.toByteData(format: ui.ImageByteFormat.png);
  await File('$_folder/$name').writeAsBytes(png!.buffer.asUint8List());
}

Future<void> _drawInlineParts() async {
  await Directory(_folder).create(recursive: true);
  await _badge('0-logo.png', 'Inline · 1x', 1, const ui.Color(0xff3a7bd5));
  await _badge('1-logo@2x.png', 'Inline · 2x srcset', 2, const ui.Color(0xff2e8b57));
}

final _mails = {
  'gmail': (
    'Gmail reply with history and an inline image',
    '<div dir="ltr"><p>Thanks, the synthetic plan works for me. The logo below is an inline part.</p>'
        '<p><img src="${_part('0-logo.png')}" srcset="${_part('0-logo.png')} 1x, ${_part('1-logo@2x.png')} 2x" '
        'width="160" height="48" alt="logo"></p><p>Alex</p></div><br>'
        '<div class="gmail_quote gmail_quote_container"><div dir="ltr" class="gmail_attr">On Mon, Nov 9, 2026 at 9:00 AM '
        'Sam &lt;sam@example.test&gt; wrote:<br></div><blockquote class="gmail_quote" style="margin:0px 0px 0px 0.8ex;'
        'border-left:1px solid rgb(204,204,204);padding-left:1ex">'
        '<p>Earlier synthetic message: shown only after the toggle opens it.</p><p>Second paragraph of the history.</p>'
        '</blockquote></div>',
  ),
  'outlook': (
    'Outlook reply (no doctype)',
    '<html><head><style>p{margin:0 0 10px;font-family:Calibri,sans-serif}</style></head><body>'
        '<div><p>Short synthetic answer written in Outlook.</p></div>'
        '<div id="appendonsend"></div><hr style="display:inline-block;width:98%">'
        '<div id="divRplyFwdMsg" dir="ltr"><b>From:</b> Sam &lt;sam@example.test&gt;<br><b>Sent:</b> Monday<br>'
        '<b>Subject:</b> Synthetic<p>Quoted Outlook history, folded.</p></div></body></html>',
  ),
  'forward': (
    'Forward that is all quote (no toggle)',
    '<blockquote type="cite"><p>Forwarded synthetic message: nothing outside the quote, so it shows whole.</p></blockquote>',
  ),
};

class WebKitMail extends ReaderMail {
  WebKitMail() {
    rows = [
      for (final (i, MapEntry(key: id, value: (subject, _))) in _mails.entries.indexed)
        engine.Conversation(
          id: id,
          account: 'reader@example.test',
          subject: subject,
          date: DateTime.utc(2026, 11, 9, 12, 10 - i).toIso8601String(),
          from: 'Sam <sam@example.test>',
          participants: ['Sam'],
          messageCount: 1,
          unread: false,
          starred: false,
          hasAttachment: false,
          mailboxes: ['inbox'],
          snippet: 'Synthetic WebKit mail',
          category: 'notification',
          waiting: false,
          labels: [],
        ),
    ];
  }

  @override
  Future<engine.Thread> show_({required String id, required int maxChars, required bool html}) async => engine.Thread(
    conversation: rows.firstWhere((c) => c.id == id),
    messages: [
      engine.Message(
        id: '$id-1',
        mailbox: 'INBOX',
        role: 'inbox',
        from: ['Sam <sam@example.test>'],
        to: ['reader@example.test'],
        cc: [],
        date: DateTime.utc(2026, 11, 9, 12).toIso8601String(),
        subject: _mails[id]!.$1,
        unread: false,
        starred: false,
        text: 'Synthetic WebKit mail',
        truncated: false,
        attachments: [],
        html: _mails[id]!.$2,
      ),
    ],
  );
}
