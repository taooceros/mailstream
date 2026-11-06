import 'package:flutter_test/flutter_test.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/backend/models.dart';
import 'package:mailstream_ui/state/mail_state.dart';

class RecordingEngine implements engine.Mailstream {
  engine.Draft? sent;
  engine.ReplyOverrides? edits;
  String? replyId;
  String? replyBody;
  bool? replyAll;

  @override
  Future<engine.Sent> send({required engine.Draft draft, required bool confirm}) async {
    expect(confirm, isTrue);
    sent = draft;
    return engine.Sent(from: draft.from, to: draft.to, subject: draft.subject);
  }

  @override
  Future<engine.Sent> reply({
    required String id,
    required String body,
    required bool all,
    required engine.ReplyOverrides? overrides,
    required bool confirm,
  }) async {
    expect(confirm, isTrue);
    replyId = id;
    replyBody = body;
    replyAll = all;
    edits = overrides;
    return engine.Sent(from: 'self@example.invalid', to: ['sender@example.invalid'], subject: 'Re: original');
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

const source = Conversation(
  id: 'thread',
  account: 'self@example.invalid',
  subject: 'original',
  date: null,
  from: Address(email: 'sender@example.invalid'),
  participants: [],
  messageCount: 1,
  unread: false,
  starred: false,
  hasAttachment: false,
  mailboxes: {},
  snippet: null,
  category: null,
  waiting: false,
  labels: [],
);

engine.Draft outgoing(ComposeDraft draft) => engine.Draft(
  from: draft.from!,
  to: draft.to.isEmpty ? [] : [draft.to],
  cc: draft.cc.isEmpty ? [] : [draft.cc],
  bcc: draft.bcc.isEmpty ? [] : [draft.bcc],
  subject: draft.subject,
  body: draft.body,
  attachments: List.of(draft.attachments),
);

void main() {
  test('new-message attachment paths reach send and close only the sent draft', () async {
    final engine = RecordingEngine();
    final state = MailState(engine);
    final draft = ComposeDraft(id: 1, from: 'self@example.invalid', to: 'recipient@example.invalid', subject: 'new', body: 'body')
      ..attachments.addAll(['/tmp/dropped.txt', '/tmp/pasted.png']);
    state.composes.add(draft);
    await state.sendCompose(draft, outgoing(draft));
    expect(engine.sent!.attachments, ['/tmp/dropped.txt', '/tmp/pasted.png']);
    expect(engine.replyId, isNull);
    expect(state.composes, isEmpty);
    state.dispose();
  });

  test('untouched reply fields keep the engine plan, attachments still reach reply', () async {
    final engine = RecordingEngine();
    final state = MailState(engine);
    final draft = ComposeDraft(
      id: 1,
      replyTo: source,
      replyAll: true,
      from: source.account,
      to: 'sender@example.invalid',
      cc: 'copy@example.invalid',
      subject: 'Re: original',
      body: 'reply',
    )..attachments.add('/tmp/pasted.png');
    state.composes.add(draft);
    await state.sendCompose(draft, outgoing(draft));
    expect(engine.replyId, source.id);
    expect(engine.replyBody, 'reply');
    expect(engine.replyAll, isTrue);
    expect(engine.edits!.to, isNull);
    expect(engine.edits!.cc, isNull);
    expect(engine.edits!.bcc, isNull);
    expect(engine.edits!.subject, isNull);
    expect(engine.edits!.attachments, ['/tmp/pasted.png']);
    expect(engine.sent, isNull);
    state.dispose();
  });

  test('reply edits include explicit clears and count as unsaved content', () async {
    final engine = RecordingEngine();
    final state = MailState(engine);
    final draft = ComposeDraft(
      id: 1,
      replyTo: source,
      from: source.account,
      to: 'sender@example.invalid',
      cc: 'copy@example.invalid',
      subject: 'Re: original',
    );
    expect(draft.hasContent, isFalse);
    draft
      ..to = 'edited@example.invalid'
      ..cc = ''
      ..bcc = 'hidden@example.invalid'
      ..subject = '';
    expect(draft.hasContent, isTrue);
    state.composes.add(draft);
    await state.sendCompose(draft, outgoing(draft));
    expect(engine.edits!.to, ['edited@example.invalid']);
    expect(engine.edits!.cc, isEmpty);
    expect(engine.edits!.bcc, ['hidden@example.invalid']);
    expect(engine.edits!.subject, '');
    state.dispose();
  });
}
