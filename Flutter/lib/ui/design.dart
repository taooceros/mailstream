import 'dart:convert';

import 'package:flutter/widgets.dart';

/// The native app's `Design` tokens (Sources/Mailstream/UI/Design.swift), ported: semantic colors
/// for both appearances, the type scale, radii, durations and the house curve. Every literal in
/// the UI comes from here.
@immutable
class Palette {
  const Palette({
    required this.page,
    required this.sidebar,
    required this.text,
    required this.muted,
    required this.faint,
    required this.line,
    required this.hover,
    required this.chip,
    required this.selection,
    required this.accent,
    required this.danger,
    required this.success,
    required this.star,
    required this.waiting,
    required this.onAccent,
  });

  /// The reading surface: the reader page, and what a sliding list uncovers.
  final Color page;
  final Color sidebar;
  final Color text;
  final Color muted;
  final Color faint;

  /// Hairlines between messages, card borders, separators.
  final Color line;

  /// Row and button hover fill.
  final Color hover;

  /// Quiet fill for chips, skeletons and thumbnails.
  final Color chip;

  /// The selected sidebar row and the keyboard highlight.
  final Color selection;
  final Color accent;
  final Color danger;
  final Color success;
  final Color star;
  final Color waiting;
  final Color onAccent;

  static const light = Palette(
    page: Color(0xFFFFFFFF),
    sidebar: Color(0xFFF1F1F3),
    text: Color(0xD9000000),
    muted: Color(0x80000000),
    faint: Color(0x40000000),
    line: Color(0x1A000000),
    hover: Color(0x0A000000),
    chip: Color(0x0F000000),
    selection: Color(0x14000000),
    accent: Color(0xFF007AFF),
    danger: Color(0xFFFF3B30),
    success: Color(0xFF34C759),
    star: Color(0xFFFFCC00),
    waiting: Color(0xFFFF9500),
    onAccent: Color(0xFFFFFFFF),
  );

  static const dark = Palette(
    page: Color(0xFF1E1E1E),
    sidebar: Color(0xFF282829),
    text: Color(0xD9FFFFFF),
    muted: Color(0x8CFFFFFF),
    faint: Color(0x40FFFFFF),
    line: Color(0x1AFFFFFF),
    hover: Color(0x0DFFFFFF),
    chip: Color(0x14FFFFFF),
    selection: Color(0x1FFFFFFF),
    accent: Color(0xFF0A84FF),
    danger: Color(0xFFFF453A),
    success: Color(0xFF30D158),
    star: Color(0xFFFFD60A),
    waiting: Color(0xFFFF9F0A),
    onAccent: Color(0xFFFFFFFF),
  );
}

/// The type scale, in points.
abstract final class TextSize {
  static const micro = 10.5;
  static const caption = 11.0;
  static const footnote = 12.0;
  static const body = 13.0;

  /// Message text in the reader.
  static const reading = 14.0;
  static const headline = 15.0;
  static const title = 20.0;

  /// The thread title.
  static const display = 27.0;
}

abstract final class FontFamily {
  /// The system font; Flutter on macOS resolves it to SF Pro.
  static const String? ui = null;
  static const serif = 'New York';
  static const serifFallback = ['Georgia', 'Times New Roman'];
  static const mono = 'SF Mono';
  static const monoFallback = ['Menlo', 'Monaco'];
}

abstract final class Corner {
  static const small = 4.0;
  static const control = 8.0;
  static const card = 10.0;
  static const panel = 14.0;
}

abstract final class Motion {
  /// Hover, press, color changes.
  static const quick = Duration(milliseconds: 150);

  /// Fades and small moves.
  static const brief = Duration(milliseconds: 240);

  /// Folds and slides.
  static const standard = Duration(milliseconds: 300);

  /// Entrances.
  static const long = Duration(milliseconds: 450);
}

/// The house easing curve: quick start, long soft landing.
const houseCurve = Cubic(0.2, 0.9, 0.25, 1);

abstract final class Shadows {
  /// Under a page sliding over another (the reader, a bundle).
  static List<BoxShadow> page(Brightness brightness) => [
    BoxShadow(color: const Color(0xFF000000).withValues(alpha: brightness == Brightness.dark ? 0.35 : 0.12), blurRadius: 14),
  ];

  static const raised = [
    BoxShadow(color: Color(0x0A000000), blurRadius: 2, offset: Offset(0, 1)),
    BoxShadow(color: Color(0x0F000000), blurRadius: 20, offset: Offset(0, 6)),
  ];

  static const floating = [
    BoxShadow(color: Color(0x0A000000), blurRadius: 2, offset: Offset(0, 1)),
    BoxShadow(color: Color(0x17000000), blurRadius: 32, offset: Offset(0, 10)),
  ];
}

/// The conversation list's fixed columns (`ListMetrics`).
abstract final class ListMetrics {
  static const rowHeight = 38.0;
  static const sectionHeight = 34.0;
  static const dotX = 14.0;
  static const senderX = 28.0;
  static const avatarSize = 28.0;
  static const senderWidth = 196.0;
  static const nameX = senderX + avatarSize + 9;
  static const subjectX = senderX + senderWidth + 16;
  static const dateWidth = 62.0;
  static const inset = 10.0;
  static const trailingInset = 14.0;
}

/// Stable per-account colors, in sidebar order (`AccountStyle`).
const accountPalette = [
  Color(0xFF007AFF), Color(0xFFFF9500), Color(0xFFAF52DE), Color(0xFF34C759), Color(0xFFFF2D55), Color(0xFF30B0C7),
];

/// Muted topic colors (`TagPalette`): light and dark variants, known topics keep fixed hues.
const _knownTopics = <String, (Color, Color)>{
  'work': (Color(0xFF3B82F6), Color(0xFF60A5FA)),
  'education': (Color(0xFFA855F7), Color(0xFFC084FC)),
  'finance': (Color(0xFF10B981), Color(0xFF34D399)),
  'shopping': (Color(0xFFF59E0B), Color(0xFFFBBF24)),
  'tech': (Color(0xFF06B6D4), Color(0xFF22D3EE)),
  'travel': (Color(0xFFEC4899), Color(0xFFF472B6)),
  'food': (Color(0xFFF97316), Color(0xFFFB923C)),
  'health': (Color(0xFF14B8A6), Color(0xFF2DD4BF)),
  'social': (Color(0xFF8B5CF6), Color(0xFFA78BFA)),
};
const _fallbackTopics = [
  (Color(0xFF64748B), Color(0xFF94A3B8)),
  (Color(0xFF6366F1), Color(0xFF818CF8)),
  (Color(0xFF84CC16), Color(0xFFA3E635)),
  (Color(0xFFE11D48), Color(0xFFFB7185)),
];

Color topicColor(String label, Brightness brightness) {
  final key = label.toLowerCase();
  final pair = _knownTopics[key] ?? _fallbackTopics[stableIndex(key, _fallbackTopics.length)];
  return brightness == Brightness.dark ? pair.$2 : pair.$1;
}

/// Sender avatar colors (`Avatar.color`): system colors by a stable hash of the address.
const _avatarLight = [
  Color(0xFF007AFF), Color(0xFFAF52DE), Color(0xFFFF2D55), Color(0xFFFF3B30), Color(0xFFFF9500), Color(0xFF34C759),
  Color(0xFF30B0C7), Color(0xFF5856D6), Color(0xFFA2845E), Color(0xFF00C7BE), Color(0xFF32ADE6),
];
const _avatarDark = [
  Color(0xFF0A84FF), Color(0xFFBF5AF2), Color(0xFFFF375F), Color(0xFFFF453A), Color(0xFFFF9F0A), Color(0xFF30D158),
  Color(0xFF40C8E0), Color(0xFF5E5CE6), Color(0xFFAC8E68), Color(0xFF63E6E2), Color(0xFF64D2FF),
];

final _avatarIndex = <String, int>{};

Color avatarColor(String email, Brightness brightness) {
  final palette = brightness == Brightness.dark ? _avatarDark : _avatarLight;
  final key = email.toLowerCase();
  return palette[_avatarIndex[key] ??= stableIndex(key, palette.length)];
}

/// A slot in `0..<count` for `key` that is the same on every launch and matches the native app
/// (FNV-1a 64 over UTF-8; Dart's native ints wrap like the Swift `&*`).
int stableIndex(String key, int count) {
  var hash = 1469598103934665603;
  for (final byte in utf8.encode(key)) {
    hash = (hash ^ byte) * 1099511628211;
  }
  return (BigInt.from(hash).toUnsigned(64) % BigInt.from(count)).toInt();
}

/// The palette and brightness in effect, from the nearest `DesignScope`.
class DesignScope extends InheritedWidget {
  const DesignScope({super.key, required this.palette, required this.brightness, required super.child});

  final Palette palette;
  final Brightness brightness;

  static DesignScope of(BuildContext context) => context.dependOnInheritedWidgetOfExactType<DesignScope>()!;

  @override
  bool updateShouldNotify(DesignScope oldWidget) => palette != oldWidget.palette || brightness != oldWidget.brightness;
}

extension DesignContext on BuildContext {
  Palette get palette => DesignScope.of(this).palette;
  Brightness get brightness => DesignScope.of(this).brightness;
}

TextStyle uiText(double size, Color color, {FontWeight weight = FontWeight.w400, double? height}) =>
    TextStyle(fontSize: size, color: color, fontWeight: weight, height: height, letterSpacing: size >= 20 ? -0.3 : -0.08);
