/// The window's values, made from the engine's typed answers (`package:mailstream`).
library;

import 'package:flutter/foundation.dart';
import 'package:mailstream/mailstream.dart' as engine;

enum MailboxRole { inbox, starred, important, sent, drafts, archive, all, junk, trash, other }

MailboxRole roleNamed(String? name) => MailboxRole.values.firstWhere((r) => r.name == name, orElse: () => MailboxRole.other);

extension MailboxRoleTitle on MailboxRole {
  String get title => switch (this) {
    MailboxRole.inbox => 'Inbox',
    MailboxRole.starred => 'Starred',
    MailboxRole.important => 'Important',
    MailboxRole.sent => 'Sent',
    MailboxRole.drafts => 'Drafts',
    MailboxRole.archive => 'Archive',
    MailboxRole.all => 'All Mail',
    MailboxRole.junk => 'Junk',
    MailboxRole.trash => 'Trash',
    MailboxRole.other => 'Folder',
  };
}

enum MailCategory { people, notification, newsletter, invitation }

extension MailCategoryTitle on MailCategory {
  String get title => switch (this) {
    MailCategory.people => 'People',
    MailCategory.notification => 'Notifications',
    MailCategory.newsletter => 'Newsletters',
    MailCategory.invitation => 'Invitations',
  };
}

class Account {
  const Account({required this.id, required this.label, required this.provider, this.services = const []});

  factory Account.of(engine.Account account) =>
      Account(id: account.id, label: account.label, provider: account.provider, services: account.services);

  /// The address; accounts are addressed by it.
  final String id;
  final String label;
  final String provider;

  /// What the sign-in grants: `mail`, `contacts`, `chat`.
  final List<String> services;

  String get title => label.isEmpty ? id : label;
}

class Mailbox {
  const Mailbox({required this.account, required this.id, required this.name, required this.role, this.unread, this.total});

  factory Mailbox.of(String account, engine.Mailbox mailbox) => Mailbox(
    account: account,
    id: mailbox.id,
    name: mailbox.name,
    role: roleNamed(mailbox.role),
    unread: mailbox.unread,
    total: mailbox.total,
  );

  final String account;
  final String id;
  final String name;
  final MailboxRole role;
  final int? unread;
  final int? total;
}

class Address {
  const Address({this.name, required this.email});

  /// `Name <email>` or a bare address, as the engine writes them.
  factory Address.parse(String raw) {
    final match = RegExp(r'^\s*(.*?)\s*<([^>]+)>\s*$').firstMatch(raw);
    if (match == null) return Address(email: raw.trim());
    var name = match.group(1)!.trim();
    if (name.length >= 2 && name.startsWith('"') && name.endsWith('"')) name = name.substring(1, name.length - 1);
    return Address(name: name.isEmpty ? null : name, email: match.group(2)!.trim());
  }

  final String? name;
  final String email;

  String get displayName => (name == null || name!.isEmpty) ? email : name!;

  /// The first letter or digit of the name, uppercased (`Avatar.initials`).
  String get initials {
    for (final rune in displayName.trim().runes) {
      final char = String.fromCharCode(rune);
      if (RegExp(r'[\p{L}\p{N}]', unicode: true).hasMatch(char)) return char.toUpperCase();
    }
    return '?';
  }
}

/// One row of a listing or a search.
class Conversation {
  const Conversation({
    required this.id,
    required this.account,
    required this.subject,
    required this.date,
    required this.from,
    required this.participants,
    required this.messageCount,
    required this.unread,
    required this.starred,
    required this.hasAttachment,
    required this.mailboxes,
    required this.snippet,
    required this.category,
    required this.waiting,
    required this.labels,
  });

  factory Conversation.of(engine.Conversation c) => Conversation(
    id: c.id,
    account: c.account,
    subject: c.subject,
    date: _date(c.date),
    from: c.from.let(Address.parse),
    participants: c.participants,
    messageCount: c.messageCount,
    unread: c.unread,
    starred: c.starred,
    hasAttachment: c.hasAttachment,
    mailboxes: c.mailboxes.map(roleNamed).toSet(),
    snippet: c.snippet,
    category: MailCategory.values.where((category) => category.name == c.category).firstOrNull,
    waiting: c.waiting,
    labels: c.labels,
  );

  final String id;
  final String account;
  final String subject;
  final DateTime? date;

  /// The latest incoming message's sender.
  final Address? from;
  final List<String> participants;
  final int messageCount;
  final bool unread;
  final bool starred;
  final bool hasAttachment;
  final Set<MailboxRole> mailboxes;
  final String? snippet;
  final MailCategory? category;
  final bool waiting;
  final List<String> labels;

  /// Service mail the Smart Inbox folds into a bundle; anything waiting on the user stays out.
  MailCategory? get bundle => switch (category) {
    MailCategory.notification || MailCategory.newsletter || MailCategory.invitation when !waiting => category,
    _ => null,
  };

  /// Whether `other` shows the same: a re-read that changed nothing keeps the row it had.
  bool sameAs(Conversation other) =>
      id == other.id &&
      account == other.account &&
      subject == other.subject &&
      date == other.date &&
      from?.email == other.from?.email &&
      from?.name == other.from?.name &&
      listEquals(participants, other.participants) &&
      messageCount == other.messageCount &&
      unread == other.unread &&
      starred == other.starred &&
      hasAttachment == other.hasAttachment &&
      setEquals(mailboxes, other.mailboxes) &&
      snippet == other.snippet &&
      category == other.category &&
      waiting == other.waiting &&
      listEquals(labels, other.labels);

  Conversation copyWith({bool? unread, bool? starred, bool? waiting, String? snippet}) => Conversation(
    id: id,
    account: account,
    subject: subject,
    date: date,
    from: from,
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
  );
}

class AttachmentInfo {
  const AttachmentInfo({required this.filename, required this.mime, required this.size, required this.path});

  factory AttachmentInfo.of(engine.Attachment a) =>
      AttachmentInfo(filename: a.filename.isEmpty ? 'attachment' : a.filename, mime: a.mime, size: a.size, path: a.path);

  final String filename;
  final String mime;
  final int size;
  final String path;
}

/// One message of a conversation, with its HTML body.
class MessageDetail {
  const MessageDetail({
    required this.id,
    required this.role,
    required this.from,
    required this.to,
    required this.cc,
    required this.date,
    required this.subject,
    required this.unread,
    required this.starred,
    required this.text,
    required this.html,
    required this.attachments,
    required this.error,
  });

  factory MessageDetail.of(engine.Message m) {
    List<Address> addresses(List<String> raw) => raw.map(Address.parse).toList();
    return MessageDetail(
      id: m.id,
      role: roleNamed(m.role),
      from: addresses(m.from),
      to: addresses(m.to),
      cc: addresses(m.cc),
      date: _date(m.date),
      subject: m.subject,
      unread: m.unread,
      starred: m.starred,
      text: m.text,
      html: m.html,
      attachments: m.attachments.map(AttachmentInfo.of).toList(),
      error: m.error,
    );
  }

  final String id;
  final MailboxRole role;
  final List<Address> from;
  final List<Address> to;
  final List<Address> cc;
  final DateTime? date;
  final String subject;
  final bool unread;
  final bool starred;
  final String? text;
  final String? html;
  final List<AttachmentInfo> attachments;
  final String? error;

  Address get sender => from.isEmpty ? const Address(email: 'unknown') : from.first;
}

/// The engine's ISO 8601 dates, in local time.
DateTime? _date(String? iso) => iso == null ? null : DateTime.tryParse(iso)?.toLocal();

extension Let<T extends Object> on T? {
  R? let<R>(R Function(T) transform) {
    final self = this;
    return self == null ? null : transform(self);
  }
}
