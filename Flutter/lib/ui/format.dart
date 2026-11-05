import 'package:intl/intl.dart';

bool _sameDay(DateTime a, DateTime b) => a.year == b.year && a.month == b.month && a.day == b.day;

DateTime _startOfWeek(DateTime d) => DateTime(d.year, d.month, d.day).subtract(Duration(days: d.weekday % 7));

/// A row's date (`ConversationRow.format`): time today, month and day this year, else numeric.
String listDate(DateTime? date, DateTime now) {
  if (date == null) return '';
  if (_sameDay(date, now)) return DateFormat.jm().format(date);
  if (date.year == now.year) return DateFormat.MMMd().format(date);
  return DateFormat.yMd().format(date);
}

/// A row's group header (`ConversationRow.section`).
String listSection(DateTime? date, DateTime now) {
  if (date == null) return 'Older';
  if (_sameDay(date, now)) return 'Today';
  if (_sameDay(date, now.subtract(const Duration(days: 1)))) return 'Yesterday';
  final week = _startOfWeek(now);
  if (!date.isBefore(week)) return 'This Week';
  if (!date.isBefore(week.subtract(const Duration(days: 7)))) return 'Last Week';
  if (date.year == now.year) return DateFormat.MMMM().format(date);
  return DateFormat.yMMMM().format(date);
}

String messageTime(DateTime? date) => date == null ? '' : DateFormat.jm().format(date);

String messageDate(DateTime? date, DateTime now) {
  if (date == null) return '';
  if (_sameDay(date, now)) return DateFormat.jm().format(date);
  if (date.year == now.year) return DateFormat('MMM d, h:mm a').format(date);
  return DateFormat('MMM d, y, h:mm a').format(date);
}

String dayDivider(DateTime date, DateTime now) {
  if (_sameDay(date, now)) return 'Today';
  if (_sameDay(date, now.subtract(const Duration(days: 1)))) return 'Yesterday';
  if (date.year == now.year) return DateFormat('EEEE, MMMM d').format(date);
  return DateFormat.yMMMMd().format(date);
}

bool sameDay(DateTime a, DateTime b) => _sameDay(a, b);

/// A subject split for the list (`Subject`): reply and forward prefixes dropped, leading `[list]`
/// tags kept apart so the row can dim them.
({String tag, String title}) splitSubject(String subject) {
  final prefix = RegExp(r'^\s*((re|fwd?|fw|aw|wg|sv|vs|antw|回复|答复|转发)(\[\d+\])?\s*[:：]\s*)', caseSensitive: false);
  var rest = subject.trim();
  var tags = <String>[];
  while (true) {
    final match = prefix.firstMatch(rest);
    if (match != null) {
      rest = rest.substring(match.end);
      continue;
    }
    final tag = RegExp(r'^\[[^\]]{1,40}\]\s*').firstMatch(rest);
    if (tag != null && tag.end < rest.length) {
      tags.add(tag.group(0)!.trim());
      rest = rest.substring(tag.end);
      continue;
    }
    break;
  }
  return (tag: tags.join(' '), title: rest.isEmpty ? '(no subject)' : rest);
}

String fileSize(int bytes) {
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).round()} KB';
  return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
}
