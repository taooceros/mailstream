import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// A listing card whose photo is its cell's `background` attribute, as Zillow's are (MS-162), and an
// "Other rentals" card whose photo and text are two `<th>`s of one row that Zillow's phone styles
// turn into blocks below 600px, stacking them (MS-165).
// Synthetic mail only: no real backend, accounts, or private mail.
// flutter run -d macos -t test/html_background_app.dart
void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final state = MailState(BackgroundMail());
  await state.start();
  runApp(MailstreamApp(state: state));
}

/// A 4×2 picture, sky over grass, that the card's `background-size: cover` stretches.
const _photo =
    'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAIAAADwyuo0AAAAE0lEQVR4nGNo3/cajhjylgbAEQDNsA5NPlHrkwAAAABJRU5ErkJggg==';

const _subject = 'Listing card with a background photo';

const _html =
    '<!DOCTYPE html><html><head><style>@media only screen and (max-width: 600px) {'
    ' .displayBlock { display: block !important; width: 100% !important }'
    ' .wrapper { width: 100% !important } }</style></head><body style="margin:0">'
    '<table width="100%" cellpadding="0" cellspacing="0" border="0"><tr><td align="center" style="padding:24px">'
    '<h2 style="font:800 24px Arial,sans-serif;margin:0 0 16px">This home has been listed for rent at \$2,000 per month</h2>'
    '<table class="cardElevated" align="center" cellpadding="0" cellspacing="0" border="0" '
    'style="width:504px;border-radius:12px;border:1px solid #cdcdd3;box-shadow:0 8px 20px rgba(0,0,0,0.15)"><tbody>'
    '<tr><td class="dmBgGray800" align="center" valign="top" background="$_photo" '
    'style="background-repeat:no-repeat;background-position:center;background-size:cover;'
    'background-color:#dddde1;border-radius:12px 12px 0 0">'
    '<a href="https://example.com/listing" style="text-decoration:none;display:block">'
    '<div style="height:240px;text-align:left;padding:8px">'
    '<span style="background:#fffffe;border-radius:20px;padding:2px 8px;font:700 12px Helvetica,Arial,sans-serif;'
    'color:#111116">For rent</span></div></a></td></tr>'
    '<tr><td style="padding:16px 24px;font:14px Helvetica,Arial,sans-serif;color:#111116;text-align:left">'
    '<b style="font-size:20px">\$2,000/mo</b><br>3 bd | 2 ba | 1,000 sqft<br>1 Example Street, Springfield</td></tr>'
    '</tbody></table>'
    '<h3 style="font:700 18px Arial,sans-serif;margin:32px 0 12px;text-align:left">Other rentals you might like</h3>'
    '<table class="wrapper" align="center" cellpadding="0" cellspacing="0" border="0" '
    'style="width:501px;border:1px solid #cdcdd3;border-radius:12px;box-shadow:0 4px 12px rgba(0,0,0,0.12)"><tr>'
    '<th class="displayBlock" width="243" valign="top" background="$_photo" '
    'style="width:243px;height:160px;background-size:cover;background-color:#dddde1;border-radius:12px 0 0 12px"></th>'
    '<th class="displayBlock" valign="top" style="padding:16px;font:400 14px Helvetica,Arial,sans-serif;'
    'color:#111116;text-align:left"><b style="font-size:20px">\$2,150/mo</b><br>2 bd | 1 ba | 850 sqft<br>'
    '2 Example Avenue, Springfield</th></tr></table>'
    '</td></tr></table></body></html>';

class BackgroundMail extends ReaderMail {
  BackgroundMail() {
    rows = [
      engine.Conversation(
        id: 'listing',
        account: 'reader@example.test',
        subject: _subject,
        date: DateTime.utc(2026, 11, 9, 12).toIso8601String(),
        from: 'Listings <listings@example.test>',
        participants: ['Listings'],
        messageCount: 1,
        unread: false,
        starred: false,
        hasAttachment: false,
        mailboxes: ['inbox'],
        snippet: 'Synthetic listing card',
        category: 'people',
        waiting: false,
        labels: [],
      ),
    ];
  }

  @override
  Future<engine.Thread> show_({required String id, required int maxChars, required bool html}) async => engine.Thread(
    conversation: rows.single,
    messages: [
      engine.Message(
        id: '$id-1',
        mailbox: 'INBOX',
        role: 'inbox',
        from: ['Listings <listings@example.test>'],
        to: ['reader@example.test'],
        cc: [],
        date: DateTime.utc(2026, 11, 9, 12).toIso8601String(),
        subject: _subject,
        unread: false,
        starred: false,
        text: 'Synthetic listing card',
        truncated: false,
        attachments: [],
        html: _html,
      ),
    ],
  );
}
