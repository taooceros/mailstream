import 'dart:convert';

import 'package:flutter/material.dart';

/// Material 3: one seed color, the scheme and type scale derived from it, Material motion. Only
/// what Material has no token for lives here: stable per-account, per-topic and per-sender colors,
/// and the list's row heights.
abstract final class MailTheme {
  static const seed = Color(0xFF0B57D0);

  static ThemeData of(Brightness brightness) {
    final scheme = ColorScheme.fromSeed(seedColor: seed, brightness: brightness);
    return ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      visualDensity: VisualDensity.standard,
      scaffoldBackgroundColor: scheme.surface,
      chipTheme: ChipThemeData(
        side: BorderSide.none,
        backgroundColor: scheme.surfaceContainerHigh,
        labelStyle: TextStyle(color: scheme.onSurfaceVariant),
        shape: const StadiumBorder(),
      ),
      cardTheme: const CardThemeData(elevation: 0, margin: EdgeInsets.zero),
      snackBarTheme: const SnackBarThemeData(behavior: SnackBarBehavior.floating, width: 520),
      tooltipTheme: const TooltipThemeData(waitDuration: Duration(milliseconds: 600)),
    );
  }
}

/// Material motion for the pages that slide over the list.
abstract final class Motion {
  static const enter = Durations.medium4;
  static const exit = Durations.medium2;
  static const curve = Easing.emphasizedDecelerate;
}

/// The conversation list's rows: dense two-line list items.
abstract final class ListMetrics {
  static const rowHeight = 60.0;
  static const sectionHeight = 30.0;
  static const avatarSize = 36.0;
  static const inset = 6.0;
}

/// Material's large-screen panes: rounded surfaces on the surface container, with a spacer
/// between them and a margin to the window edge (tightened for a desktop mail client).
abstract final class Panes {
  static const margin = 8.0;
  static const spacer = 8.0;
  static const corner = 16.0;
}

/// Stable per-account colors, in sidebar order.
const accountPalette = [
  Color(0xFF0B57D0), Color(0xFFE8710A), Color(0xFF9334E6), Color(0xFF188038), Color(0xFFD93025), Color(0xFF007B83),
];

/// Topic colors: known topics keep fixed hues, the rest take a stable one.
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

/// Sender avatars: Material tonal pairs (container, on-container), by a stable hash of the address.
const _avatarSeeds = [
  Color(0xFF0B57D0), Color(0xFF9334E6), Color(0xFFD01884), Color(0xFFD93025), Color(0xFFE8710A), Color(0xFF188038),
  Color(0xFF007B83), Color(0xFF5B5FC7), Color(0xFF8D6E63), Color(0xFF00897B), Color(0xFF1E88E5),
];
final _avatarSchemes = <(int, Brightness), ColorScheme>{};
final _avatarIndex = <String, int>{};

({Color background, Color foreground}) avatarColors(String email, Brightness brightness) {
  final key = email.toLowerCase();
  final index = _avatarIndex[key] ??= stableIndex(key, _avatarSeeds.length);
  final scheme = _avatarSchemes[(index, brightness)] ??=
      ColorScheme.fromSeed(seedColor: _avatarSeeds[index], brightness: brightness);
  return (background: scheme.primaryContainer, foreground: scheme.onPrimaryContainer);
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

extension ThemeContext on BuildContext {
  ColorScheme get colors => Theme.of(this).colorScheme;
  TextTheme get text => Theme.of(this).textTheme;
  Brightness get brightness => Theme.of(this).brightness;
}
