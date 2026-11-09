import 'dart:convert';
import 'dart:developer';
import 'dart:io';
import 'dart:isolate';

import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/main.dart';
import 'package:mailstream_ui/state/mail_state.dart';

import 'reader_fixture.dart';

// The real shell over 320 synthetic conversations, each a news mail with about 100 KB of HTML:
// it opens and closes 300 of them one after another, as clicking a row and Esc do, and prints the
// Dart heap after a full collection, read from its own VM service (what DevTools' Memory page
// shows), after loading, after 100 and 300 conversations and after 60 idle seconds, then quits.
// A profile build has the VM service and release's optimization (MS-154):
// flutter build macos --profile -t test/memory_app.dart
// "build/macos/Build/Products/Profile/Mailstream Flutter.app/Contents/MacOS/Mailstream Flutter"
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  final mail = MemoryMail()..rows = [for (var i = 0; i < 320; i++) conversation('synthetic-$i', minute: i)];
  final state = MailState(mail);
  await state.start();
  runApp(MailstreamApp(state: state));
  await Future<void>.delayed(const Duration(seconds: 3));
  await report('loaded');
  final conversations = state.conversations.toList();
  for (var i = 0; i < 300; i++) {
    final c = conversations[i];
    state.open(c);
    await state.thread(c).future;
    // The reader draws its bodies, then fades out.
    await Future<void>.delayed(const Duration(milliseconds: 250));
    state.close();
    await Future<void>.delayed(const Duration(milliseconds: 250));
    if (i == 99) await report('opened 100');
  }
  await report('opened 300');
  await Future<void>.delayed(const Duration(seconds: 60));
  await report('idle 60 s');
  exit(0);
}

/// One line: the heap in use and the instances holding conversations' bodies.
Future<void> report(String label) async {
  final uri = (await Service.getInfo()).serverUri;
  final isolate = Service.getIsolateId(Isolate.current);
  if (uri == null || isolate == null) {
    stdout.writeln('memory $label: no VM service (build in profile mode)');
    return;
  }
  final client = HttpClient();
  try {
    final request = await client.getUrl(uri.resolve('getAllocationProfile?isolateId=$isolate&gc=true'));
    final body = await (await request.close()).transform(utf8.decoder).join();
    final json = jsonDecode(body) as Map<String, Object?>;
    final profile = (json['result'] ?? json) as Map<String, Object?>;
    final usage = profile['memoryUsage'] as Map<String, Object?>;
    String mb(Object? bytes) => '${((bytes as num) / 1e6).toStringAsFixed(1)} MB';
    final counts = <String, String>{};
    for (final member in (profile['members'] as List).cast<Map<String, Object?>>()) {
      final name = (member['class'] as Map<String, Object?>)['name'];
      if (name case 'ThreadEntry' || 'MessageDetail') counts['$name'] = '${member['instancesCurrent']}';
      if (name == '_OneByteString' || name == '_TwoByteString') counts['$name'] = mb(member['bytesCurrent']);
    }
    stdout.writeln('memory $label: heap ${mb(usage['heapUsage'])} $counts');
  } finally {
    client.close();
  }
}

/// Conversations' bodies as the engine answers them: new strings on every answer, as over FFI.
class MemoryMail extends ReaderMail {
  @override
  Future<engine.Thread> show_({required String id, required int maxChars, required bool html}) async {
    final c = rows.firstWhere((r) => r.id == id);
    final m = message(id, 1);
    return engine.Thread(
      conversation: c,
      messages: [
        engine.Message(
          id: m.id,
          mailbox: m.mailbox,
          role: m.role,
          from: m.from,
          to: m.to,
          cc: m.cc,
          date: m.date,
          subject: m.subject,
          unread: false,
          starred: false,
          text: 'Synthetic news digest for $id.',
          html: news(id),
          truncated: false,
          attachments: [],
        ),
      ],
    );
  }

  /// About 100 KB of a newsletter's markup: tables of stories with links.
  static String news(String id) {
    final html = StringBuffer('<html><body><table width="600" style="font-family:Helvetica">');
    for (var story = 0; html.length < 100000; story++) {
      html.write(
        '<tr><td style="padding:12px;border-bottom:1px solid #ddd"><h2 style="margin:0">'
        'Story $story of $id</h2><p style="color:#444;line-height:1.4">Synthetic paragraph $story '
        'for $id: markets, weather and local news, written so the body runs as long as a real '
        'digest.</p><a href="https://example.test/$id/$story">Read more</a></td></tr>',
      );
    }
    html.write('</table></body></html>');
    return html.toString();
  }
}
