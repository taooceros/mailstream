/// What the window adds to the engine's typed answers (`package:mailstream`), which it uses as
/// they come: titles, the Smart Inbox's rules, and the optimistic copies of a row.
library;

import 'package:flutter/foundation.dart';
import 'package:mailstream/mailstream.dart' as engine;

extension MailboxRoleTitle on engine.MailboxRole {
  String get title => switch (this) {
    engine.MailboxRole.inbox => 'Inbox',
    engine.MailboxRole.starred => 'Starred',
    engine.MailboxRole.important => 'Important',
    engine.MailboxRole.sent => 'Sent',
    engine.MailboxRole.drafts => 'Drafts',
    engine.MailboxRole.archive => 'Archive',
    engine.MailboxRole.all => 'All Mail',
    engine.MailboxRole.junk => 'Junk',
    engine.MailboxRole.trash => 'Trash',
    engine.MailboxRole.other => 'Folder',
  };
}

extension CategoryTitle on engine.Category {
  String get title => switch (this) {
    engine.Category.people => 'People',
    engine.Category.notification => 'Notifications',
    engine.Category.newsletter => 'Newsletters',
    engine.Category.invitation => 'Invitations',
  };
}

extension AccountTitle on engine.Account {
  String get title => label.isEmpty ? id : label;
}

extension AddressName on engine.Address {
  String get displayName => (name == null || name!.isEmpty) ? email : name!;
}

/// `Name <email>` or a bare address, as the engine writes them (a sent message's recipients) and
/// people type them.
engine.Address parseAddress(String raw) {
  final match = RegExp(r'^\s*(.*?)\s*<([^>]+)>\s*$').firstMatch(raw);
  if (match == null) return engine.Address(email: raw.trim());
  var name = match.group(1)!.trim();
  if (name.length >= 2 && name.startsWith('"') && name.endsWith('"')) name = name.substring(1, name.length - 1);
  return engine.Address(name: name.isEmpty ? null : name, email: match.group(2)!.trim());
}

final _avatars = <(String, String), engine.Avatar>{};

/// [address]'s avatar by the engine's rules (MS-235), for people no answer carried one for:
/// asked once per person and kept.
engine.Avatar avatarOf(engine.Address address) {
  final name = address.displayName;
  final key = address.email.isNotEmpty ? address.email : name;
  return _avatars[(name, key)] ??= engine.avatar(name: name, key: key);
}

/// One row of a listing or a search.
extension ConversationView on engine.Conversation {
  /// [date] in local time.
  DateTime? get dateTime => _date(date);

  /// [from]'s avatar, as the answer carried it.
  engine.Avatar? get fromAvatar => from == null ? null : avatar;

  /// Service mail the Smart Inbox folds into a bundle; anything waiting on the user stays out.
  engine.Category? get bundle => switch (category) {
    engine.Category.notification || engine.Category.newsletter || engine.Category.invitation when !waiting => category,
    _ => null,
  };

  /// Whether it can be put on "Waiting on you" or resolved: tagged mail other than newsletters.
  bool get canMarkWaiting => category != null && category != engine.Category.newsletter;

  /// Whether `other` shows the same: a re-read that changed nothing keeps the row it had.
  bool sameAs(engine.Conversation other) =>
      id == other.id &&
      account == other.account &&
      subject == other.subject &&
      date == other.date &&
      from?.email == other.from?.email &&
      from?.name == other.from?.name &&
      fromMe == other.fromMe &&
      started == other.started &&
      listEquals(participants, other.participants) &&
      messageCount == other.messageCount &&
      unread == other.unread &&
      starred == other.starred &&
      hasAttachment == other.hasAttachment &&
      listEquals(mailboxes, other.mailboxes) &&
      snippet == other.snippet &&
      category == other.category &&
      waiting == other.waiting &&
      listEquals(labels, other.labels) &&
      when == other.when &&
      section == other.section;

  /// It as the window shows it before the engine confirms an action.
  engine.Conversation copyWith({bool? unread, bool? starred, bool? waiting, String? snippet}) => engine.Conversation(
    id: id,
    account: account,
    subject: subject,
    date: date,
    from: from,
    fromMe: fromMe,
    participants: participants,
    messageCount: messageCount,
    unread: unread ?? this.unread,
    starred: starred ?? this.starred,
    hasAttachment: hasAttachment,
    mailboxes: mailboxes,
    snippet: snippet ?? this.snippet,
    category: category,
    waiting: waiting ?? this.waiting,
    labels: labels,
    started: started,
    tag: tag,
    title: title,
    when: when,
    section: section,
    avatar: avatar,
  );
}

final _unknown = engine.Address(email: 'unknown');

extension MessageSender on engine.Message {
  engine.Address get sender => from.isEmpty ? _unknown : from.first;

  /// [sender]'s avatar, as the answer carried it.
  engine.Avatar? get senderAvatar => from.isEmpty ? null : avatar;
}

extension AttachmentName on engine.Attachment {
  /// [filename], or a stand-in when the part has none.
  String get name => filename.isEmpty ? 'attachment' : filename;
}

/// The engine's ISO 8601 dates, in local time.
DateTime? _date(String? iso) => iso == null ? null : DateTime.tryParse(iso)?.toLocal();
