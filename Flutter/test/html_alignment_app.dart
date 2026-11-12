import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// Native composition regression: no real backend, accounts, or private mail.
// flutter run -d macos -t test/html_alignment_app.dart
void main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  final state = MailState(AlignmentMail());
  await state.start();
  runApp(MailstreamApp(state: state));
}

class AlignmentMail extends ReaderMail {
  @override
  Future<engine.Thread> show_({
    required String id,
    required int maxChars,
    required bool html,
  }) async => engine.Thread(
    conversation: rows.firstWhere((c) => c.id == id),
    messages: [
      engine.Message(
        id: '$id-alignment',
        mailbox: 'INBOX',
        role: engine.MailboxRole.inbox,
        avatar: engine.avatar(name: 'Alex', key: 'alex@example.test'),
        from: [engine.Address(name: 'Alex', email: 'alex@example.test')],
        to: [engine.Address(email: 'reader@example.test')],
        cc: [],
        date: '2026-11-06T12:00:00Z',
        subject: 'Complete left-edge content',
        unread: false,
        starred: false,
        text: 'Synthetic left-edge regression',
        truncated: false,
        attachments: [],
        html:
            '<div dir="ltr"><div>${'🪔' * 22}<br><br><b>Complete leading characters</b><p>Left aligned paragraph and <a href="https://example.test">complete link</a></p><p>Paragraph starts at the same edge.</p></div></div>'
            '${id == 'a' ? '<p>Left edge stays aligned while scrolling.</p>' * 16 : ''}',
      ),
    ],
  );
}
