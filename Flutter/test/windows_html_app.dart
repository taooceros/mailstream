import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// Windows HTML bodies (WebView2, MS-106) with synthetic mail only: no backend, account or private
// mail. The body has styled tables, an inline image in a cached file (as the engine writes `cid:`
// parts), a 760 px table that zooms out to fit, mail scripts and handlers that must not run, a
// `target="_blank"` link and quoted history that folds.
// flutter build windows --release -t test/windows_html_app.dart
void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final logo = await _logo();
  final state = MailState(WindowsHtmlMail(logo));
  await state.start();
  runApp(MailstreamApp(state: state));
  state.open(state.conversations.first);
}

/// A 96 px picture in a temporary file, standing in for an inline part in the cache.
Future<String> _logo() async {
  final recorder = ui.PictureRecorder();
  final canvas = ui.Canvas(recorder);
  canvas.drawCircle(const ui.Offset(48, 48), 46, ui.Paint()..color = const ui.Color(0xFF3A7AFE));
  canvas.drawCircle(const ui.Offset(48, 48), 22, ui.Paint()..color = const ui.Color(0xFFFFFFFF));
  final image = await recorder.endRecording().toImage(96, 96);
  final png = await image.toByteData(format: ui.ImageByteFormat.png);
  final file = File('${Directory.systemTemp.path}${Platform.pathSeparator}mailstream-demo-logo.png');
  await file.writeAsBytes(png!.buffer.asUint8List());
  return Uri.file(file.path).toString();
}

class WindowsHtmlMail extends ReaderMail {
  WindowsHtmlMail(this.logo);

  final String logo;

  @override
  Future<engine.Thread> show_({required String id, required int maxChars, required bool html}) async => engine.Thread(
    conversation: rows.firstWhere((c) => c.id == id),
    messages: [
      engine.Message(
        id: '$id-html',
        mailbox: 'INBOX',
        role: engine.MailboxRole.inbox,
        avatar: engine.avatar(name: 'Example Weekly', key: 'news@example.test'),
        from: [engine.Address(name: 'Example Weekly', email: 'news@example.test')],
        to: [engine.Address(email: 'reader@example.test')],
        cc: [],
        date: '2026-11-08T09:00:00Z',
        subject: 'HTML body on Windows',
        unread: false,
        starred: false,
        text: '',
        truncated: false,
        attachments: [],
        html: _html(logo),
      ),
    ],
  );
}

String _html(String logo) => '''
<html><head><style>
  .card { background: #f4f7ff; border-radius: 10px; padding: 14px 18px; }
  h1 { font-size: 22px; color: #1d3c8f; margin: 0 0 6px; }
  .wide td { border: 1px solid #c9d3ea; padding: 8px; text-align: center; }
</style></head>
<body onload="document.getElementById('js').textContent = 'FAIL: onload ran'">
<table width="100%" cellpadding="0" cellspacing="0"><tr>
  <td width="110"><img src="$logo" width="96" height="96" alt="inline image"></td>
  <td class="card"><h1>Weekly digest</h1>
    <p>This body is <b>HTML</b>, drawn by <i>WebView2</i> with the image on the left loaded from a
    <code>file://</code> part in the cache.</p></td>
</tr></table>
<p id="js">Scripts: off (this line stays as it is).</p>
<script>document.getElementById('js').textContent = 'FAIL: a mail script ran';</script>
<p><a href="https://example.test/read" target="_blank">A link with target=_blank</a> opens in the
browser. <a href="javascript:document.body.innerHTML='FAIL'">A javascript: link</a> does nothing.</p>
<table class="wide" width="760" cellspacing="0"><tr>
  <td style="background:#ffe9c7">760 px</td><td style="background:#d9f5df">wide table</td>
  <td style="background:#e3e0ff">zooms out</td><td style="background:#ffd9e2">to fit</td>
</tr></table>
<ul><li>Lists</li><li>and <span style="color:#c0392b">colored</span> text</li></ul>
<div class="gmail_quote">On Fri, Example wrote:<blockquote>Earlier message, folded behind the toggle.</blockquote></div>
</body></html>''';
