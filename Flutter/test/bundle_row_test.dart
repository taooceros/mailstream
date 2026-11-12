import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/ui/conversation_list.dart';

engine.Conversation _mail(String id, String sender, {required int minute, bool unread = false}) => engine.Conversation(
  id: id,
  account: 'reader@example.test',
  subject: 'Subject $id',
  date: DateTime.utc(2026, 11, 6, 12, minute).toIso8601String(),
  from: engine.Address(name: sender, email: '${sender.toLowerCase()}@example.test'),
  participants: [sender],
  messageCount: 1,
  unread: unread,
  starred: false,
  hasAttachment: false,
  mailboxes: const [engine.MailboxRole.inbox],
  snippet: null,
  category: engine.Category.notification,
  waiting: false,
  labels: const [],
  title: 'Subject $id',
  avatar: engine.Avatar(initial: sender[0], slot: 0),
);

void main() {
  Future<String> summary(WidgetTester tester, List<engine.Conversation> newestFirst) async {
    await tester.pumpWidget(
      MacosTheme(
        data: MacosThemeData.light(),
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              width: 2000,
              child: BundleRow(
                bundle: BundleItem(engine.Category.notification, newestFirst),
                highlight: ListHighlight(),
                hover: ListHighlight(),
                next: null,
                onTap: () {},
              ),
            ),
          ),
        ),
      ),
    );
    final lines = tester.widgetList<RichText>(find.byType(RichText)).map((t) => t.text.toPlainText());
    return lines.firstWhere((line) => line.contains('  —  '));
  }

  testWidgets('a bundle names the senders of its unread mail, newest first, before newer read mail', (tester) async {
    final line = await summary(tester, [
      _mail('5', 'Ann', minute: 5),
      _mail('4', 'Ben', minute: 4),
      _mail('3', 'Cara', minute: 3, unread: true),
      _mail('2', 'Ann', minute: 2),
      _mail('1', 'Dev', minute: 1, unread: true),
    ]);
    expect(line, 'Cara, Dev  —  Subject 3');
    expect(find.text('2 new'), findsOneWidget);
  });

  testWidgets('a bundle with nothing unread names its latest senders', (tester) async {
    final line = await summary(tester, [
      _mail('3', 'Ann', minute: 3),
      _mail('2', 'Ben', minute: 2),
      _mail('1', 'Ann', minute: 1),
    ]);
    expect(line, 'Ann, Ben  —  Subject 3');
    expect(find.text('3'), findsOneWidget);
  });
}
