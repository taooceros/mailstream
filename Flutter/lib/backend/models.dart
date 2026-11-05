/// Plain values decoded from the control socket's JSON answers.
library;

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
  const Account({required this.id, required this.label, required this.provider});

  factory Account.fromJson(Map<String, Object?> json) =>
      Account(id: json['id'] as String, label: json['label'] as String? ?? '', provider: json['provider'] as String? ?? '');

  /// The address; accounts are addressed by it.
  final String id;
  final String label;
  final String provider;

  String get title => label.isEmpty ? id : label;
}

class Mailbox {
  const Mailbox({required this.account, required this.id, required this.name, required this.role, this.unread, this.total});

  factory Mailbox.fromJson(String account, Map<String, Object?> json) => Mailbox(
    account: account,
    id: json['id'] as String,
    name: json['name'] as String? ?? '',
    role: roleNamed(json['role'] as String?),
    unread: json['unread'] as int?,
    total: json['total'] as int?,
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

  /// `Name <email>` or a bare address, as the control socket writes them.
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

/// One row of `mail list` / `mail search`.
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

  factory Conversation.fromJson(Map<String, Object?> json) => Conversation(
    id: json['id'] as String,
    account: json['account'] as String,
    subject: json['subject'] as String? ?? '',
    date: DateTime.tryParse(json['date'] as String? ?? '')?.toLocal(),
    from: (json['from'] as String?).let(Address.parse),
    participants: (json['participants'] as List?)?.cast<String>() ?? const [],
    messageCount: json['messageCount'] as int? ?? 1,
    unread: json['unread'] as bool? ?? false,
    starred: json['starred'] as bool? ?? false,
    hasAttachment: json['hasAttachment'] as bool? ?? false,
    mailboxes: ((json['mailboxes'] as List?)?.cast<String>() ?? const []).map(roleNamed).toSet(),
    snippet: json['snippet'] as String?,
    category: MailCategory.values.where((c) => c.name == json['category']).firstOrNull,
    waiting: json['waiting'] as bool? ?? false,
    labels: (json['labels'] as List?)?.cast<String>() ?? const [],
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

  Conversation copyWith({bool? unread, bool? starred, bool? waiting}) => Conversation(
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
    snippet: snippet,
    category: category,
    waiting: waiting ?? this.waiting,
    labels: labels,
  );
}

class AttachmentInfo {
  const AttachmentInfo({required this.filename, required this.mime, required this.size, required this.path});

  factory AttachmentInfo.fromJson(Map<String, Object?> json) => AttachmentInfo(
    filename: json['filename'] as String? ?? 'attachment',
    mime: json['mime'] as String? ?? '',
    size: json['size'] as int? ?? 0,
    path: json['path'] as String? ?? '',
  );

  final String filename;
  final String mime;
  final int size;
  final String path;
}

/// One message of `mail show --html`.
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

  factory MessageDetail.fromJson(Map<String, Object?> json) {
    List<Address> addresses(String key) => ((json[key] as List?)?.cast<String>() ?? const []).map(Address.parse).toList();
    return MessageDetail(
      id: json['id'] as String,
      role: roleNamed(json['role'] as String?),
      from: addresses('from'),
      to: addresses('to'),
      cc: addresses('cc'),
      date: DateTime.tryParse(json['date'] as String? ?? '')?.toLocal(),
      subject: json['subject'] as String? ?? '',
      unread: json['unread'] as bool? ?? false,
      starred: json['starred'] as bool? ?? false,
      text: json['text'] as String?,
      html: json['html'] as String?,
      attachments: ((json['attachments'] as List?) ?? const []).map((a) => AttachmentInfo.fromJson(a as Map<String, Object?>)).toList(),
      error: json['error'] as String?,
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

extension Let<T extends Object> on T? {
  R? let<R>(R Function(T) transform) {
    final self = this;
    return self == null ? null : transform(self);
  }
}
