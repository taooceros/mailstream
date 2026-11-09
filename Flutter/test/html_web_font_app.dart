import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// Web fonts in Blitz (MS-164): one mail links a Google Fonts stylesheet for Lobster, a script font
// that is easy to tell from the system font; another names the same family without linking it,
// so it stays in the system font (each body has fonts of its own). The stylesheet and font are
// read from the network (fonts.googleapis.com, fonts.gstatic.com) and kept in the engine's cache
// folder, `reader-resources` under ~/Library/Caches/dev.mailstream.Mailstream; launch with
// another HOME for an empty cache.
// Synthetic mail only: no real backend, accounts, or private mail.
// flutter run -d macos -t test/html_web_font_app.dart
void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final state = MailState(WebFontMail());
  await state.start();
  runApp(MailstreamApp(state: state));
}

const _title = 'font-family:Lobster,sans-serif;font-size:40px;line-height:1.3;margin:24px 24px 8px';
const _text = 'font:16px/1.5 -apple-system,sans-serif;margin:0 24px 24px;color:#333';

final _mails = {
  'web-font': (
    subject: 'Web font: Lobster from Google Fonts',
    html:
        '<!DOCTYPE html><html><head>'
        '<link href="https://fonts.googleapis.com/css2?family=Lobster&display=swap" rel="stylesheet"></head>'
        '<body><div style="$_title">MS164 web font check</div>'
        '<p style="$_text">The title above is drawn in Lobster, a script font, once it arrives. '
        'Until then it is in the system font.</p></body></html>',
  ),
  'no-link': (
    subject: 'Same family, nothing linked',
    html:
        '<!DOCTYPE html><html><head></head>'
        '<body><div style="$_title">MS164 no web font</div>'
        '<p style="$_text">This mail names Lobster too but links no stylesheet, so its title stays '
        'in the system font even after another mail loaded Lobster.</p></body></html>',
  ),
};

class WebFontMail extends ReaderMail {
  WebFontMail() {
    rows = [
      for (final MapEntry(key: id, value: mail) in _mails.entries)
        engine.Conversation(
          id: id,
          account: 'reader@example.test',
          subject: mail.subject,
          date: DateTime.utc(2026, 11, 9, 12).toIso8601String(),
          from: 'Fonts <fonts@example.test>',
          participants: ['Fonts'],
          messageCount: 1,
          unread: false,
          starred: false,
          hasAttachment: false,
          mailboxes: ['inbox'],
          snippet: 'Synthetic web font mail',
          category: 'people',
          waiting: false,
          labels: [],
        ),
    ];
  }

  @override
  Future<engine.Thread> show_({required String id, required int maxChars, required bool html}) async => engine.Thread(
    conversation: rows.firstWhere((row) => row.id == id),
    messages: [
      engine.Message(
        id: '$id-1',
        mailbox: 'INBOX',
        role: 'inbox',
        from: ['Fonts <fonts@example.test>'],
        to: ['reader@example.test'],
        cc: [],
        date: DateTime.utc(2026, 11, 9, 12).toIso8601String(),
        subject: _mails[id]!.subject,
        unread: false,
        starred: false,
        text: 'Synthetic web font mail',
        truncated: false,
        attachments: [],
        html: _mails[id]!.html,
      ),
    ],
  );
}
