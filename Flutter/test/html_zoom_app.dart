import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// WebKit bodies wider than the reader, zoomed out to fit, keep their height (MS-78, MS-160).
// Synthetic mail only: no real backend, accounts, or private mail.
// MAILSTREAM_READER_RENDERER=webKit flutter run -d macos -t test/html_zoom_app.dart
void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final state = MailState(ZoomMail());
  await state.start();
  runApp(MailstreamApp(state: state));
}

const _pixel = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

String _rows(int count) => [
  for (var i = 1; i <= count; i++)
    '<tr><td style="padding:14px 24px;font:15px Helvetica,Arial,sans-serif;color:#202020">'
        'Row $i of the synthetic newsletter, laid out 900 px wide.</td></tr>',
].join();

/// Each a layout that grew without end once zoomed out: a standards-mode newsletter whose doctype
/// preparation broke, and layouts in quirks mode, where `html`, `body` and 100% tables stretched
/// to the view.
final _mails = {
  'xhtml': (
    'XHTML newsletter (doctype with identifiers)',
    '<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" '
        '"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">'
        '<html xmlns="http://www.w3.org/1999/xhtml"><head><style type="text/css">'
        'body,#bodyTable{height:100% !important;margin:0;padding:0;width:100% !important}'
        '</style></head><body><center>'
        '<table align="center" border="0" cellpadding="0" cellspacing="0" height="100%" width="100%" id="bodyTable" bgcolor="#eeeeee">'
        '<tr><td align="center" valign="top" style="padding:10px">'
        '<table border="0" cellpadding="0" cellspacing="0" width="900" bgcolor="#ffffff">'
        '<tr><td><img src="$_pixel" width="900" height="60" style="display:block;height:60px;background:#4a7"></td></tr>'
        '${_rows(6)}</table></td></tr></table></center></body></html>',
  ),
  'html4': (
    'HTML 4.01 mail with a 100% wrapper',
    '<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN" "http://www.w3.org/TR/html4/loose.dtd">'
        '<html><head><style>html,body{height:100%}</style></head><body style="margin:0">'
        '<div style="min-height:100%;background:#eef">'
        '<table width="100%" height="100%" border="0" cellpadding="0"><tr><td align="center" valign="middle" height="100%">'
        '<table width="900" border="0" cellpadding="12" bgcolor="#ffffff">${_rows(2)}</table>'
        '</td></tr></table></div></body></html>',
  ),
  'quirks': (
    'Notification without a doctype',
    '<html><head><style>body{margin:0;background:#f4f4f4}</style></head><body>'
        '<table width="100%" height="100%" cellpadding="0" cellspacing="0" border="0" bgcolor="#f4f4f4" style="height:100%">'
        '<tr><td align="center" valign="top">'
        '<table width="900" cellpadding="0" cellspacing="0" border="0" bgcolor="#ffffff">${_rows(24)}</table>'
        '</td></tr></table></body></html>',
  ),
};

class ZoomMail extends ReaderMail {
  ZoomMail() {
    rows = [
      for (final (i, MapEntry(key: id, value: (subject, _))) in _mails.entries.indexed)
        engine.Conversation(
          id: id,
          account: 'reader@example.test',
          subject: subject,
          date: DateTime.utc(2026, 11, 9, 12, 10 - i).toIso8601String(),
          from: 'Newsletter <news@example.test>',
          participants: ['Newsletter'],
          messageCount: 1,
          unread: false,
          starred: false,
          hasAttachment: false,
          mailboxes: ['inbox'],
          snippet: 'Synthetic wide mail',
          category: 'people',
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
        from: ['Newsletter <news@example.test>'],
        to: ['reader@example.test'],
        cc: [],
        date: DateTime.utc(2026, 11, 9, 12).toIso8601String(),
        subject: _mails[id]!.$1,
        unread: false,
        starred: false,
        text: 'Synthetic wide mail',
        truncated: false,
        attachments: [],
        html: _mails[id]!.$2,
      ),
    ],
  );
}
