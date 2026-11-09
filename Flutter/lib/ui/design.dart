import 'dart:convert';

import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

/// The Mac's look, all macos_ui: its theme ([macos]) styles the controls, its typography
/// (`context.text`) sets the type, and [MailColors] (`context.colors`) carries the semantic
/// colors the app paints with. The other classes here hold what no theme carries: motion, the
/// list's row heights, the islands' metrics, stable account and topic colors.
abstract final class MailTheme {
  static MacosThemeData macos(Brightness brightness) => brightness == Brightness.light
      ? MacosThemeData.light(accentColor: AccentColor.blue)
      : MacosThemeData.dark(accentColor: AccentColor.blue);
}

/// The Mac's semantic colors, light and dark: labels, fills, separators, backgrounds and the
/// system accents.
final class MailColors {
  const MailColors._({
    required this.brightness,
    required this.window,
    required this.background,
    required this.tray,
    required this.label,
    required this.secondaryLabel,
    required this.border,
    required this.separator,
    required this.fill,
    required this.strongFill,
    required this.selection,
    required this.accent,
    required this.accentFill,
    required this.onAccentFill,
    required this.waiting,
    required this.waitingFill,
    required this.onWaitingFill,
    required this.red,
    required this.redFill,
    required this.onRedFill,
    required this.toast,
    required this.onToast,
    required this.card,
    required this.cardBorder,
    required this.raisedCard,
    required this.spotlight,
  });

  static const light = MailColors._(
    brightness: Brightness.light,
    window: Color(0xFFE4E5EA),
    background: MacosColors.white,
    tray: Color(0xFFF5F6FA),
    label: Color(0xFF1D1D1F),
    secondaryLabel: Color(0xFF6E6E73),
    border: Color(0xFFC7C7CC),
    separator: Color(0xFFE5E5EA),
    fill: Color(0xFFF5F5F7),
    strongFill: Color(0xFFE5E5EA),
    selection: Color(0xFFE4E4E8),
    accent: Color(0xFF007AFF),
    accentFill: Color(0xFFE3EFFF),
    onAccentFill: Color(0xFF0040A0),
    waiting: Color(0xFF5856D6),
    waitingFill: Color(0xFFECEBFB),
    onWaitingFill: Color(0xFF3634A3),
    red: Color(0xFFFF3B30),
    redFill: Color(0xFFFFE8E6),
    onRedFill: Color(0xFFB3261E),
    toast: Color(0xFF2C2C2E),
    onToast: Color(0xFFF5F5F7),
    card: MacosColors.white,
    cardBorder: Color(0x14000000),
    raisedCard: MacosColors.white,
    spotlight: Color(0xA6FFFFFF),
  );

  static const dark = MailColors._(
    brightness: Brightness.dark,
    window: Color(0xFF2A2A2D),
    background: Color(0xFF1E1E1E),
    tray: Color(0xFF18181A),
    label: Color(0xFFF5F5F7),
    secondaryLabel: Color(0xFF98989D),
    border: Color(0xFF545458),
    separator: Color(0xFF38383A),
    fill: Color(0xFF2A2A2C),
    strongFill: Color(0xFF3A3A3C),
    selection: Color(0xFF3A3A3C),
    accent: Color(0xFF0A84FF),
    accentFill: Color(0xFF0F2D52),
    onAccentFill: Color(0xFFB8D8FF),
    waiting: Color(0xFF7D7AFF),
    waitingFill: Color(0xFF26254D),
    onWaitingFill: Color(0xFFD0CFFF),
    red: Color(0xFFFF453A),
    redFill: Color(0xFF4D1D1A),
    onRedFill: Color(0xFFFFB4AB),
    toast: Color(0xFFE5E5EA),
    onToast: Color(0xFF1D1D1F),
    card: Color(0xFF232327),
    cardBorder: Color(0x1AFFFFFF),
    raisedCard: Color(0xFF2C2C32),
    spotlight: Color(0x06FFFFFF),
  );

  final Brightness brightness;

  /// The window behind the islands where no system glass is under the Flutter view (Windows):
  /// set apart from [background] and [tray], as the Mac's glass reads behind the islands.
  final Color window;

  /// Islands, sheets and fields.
  final Color background;

  /// The list islands, a shade under [background], so rows can lift off them.
  final Color tray;
  final Color label;
  final Color secondaryLabel;

  /// Field and control outlines.
  final Color border;
  final Color separator;

  /// Quiet fills (message cards, the chat composer) and stronger ones (tags, selected pills).
  final Color fill;
  final Color strongFill;

  /// The unemphasized selection gray of rows.
  final Color selection;
  final Color accent;
  final Color accentFill;
  final Color onAccentFill;

  /// "Waiting on you": the system indigo.
  final Color waiting;
  final Color waitingFill;
  final Color onWaitingFill;
  final Color red;
  final Color redFill;
  final Color onRedFill;

  /// Notices floating over the window.
  final Color toast;
  final Color onToast;

  /// Cards resting on a [tray] (the calendar's events): white, dark gray in dark mode, with a
  /// [cardBorder] hairline; [raisedCard] is one raised a step further (today's date).
  final Color card;
  final Color cardBorder;
  final Color raisedCard;

  /// The wash over today's column of the timeline, lighter than the tray.
  final Color spotlight;

  /// Hover and press washes over a surface.
  Color get hover => label.withValues(alpha: brightness == Brightness.light ? 0.05 : 0.08);
  Color get pressed => label.withValues(alpha: brightness == Brightness.light ? 0.1 : 0.14);

  /// A resting [card]'s shadow, one soft layer.
  List<BoxShadow> get cardShadow => brightness == Brightness.light
      ? const [BoxShadow(color: Color(0x0F0F172A), blurRadius: 3, offset: Offset(0, 1))]
      : const [BoxShadow(color: Color(0x33000000), blurRadius: 3, offset: Offset(0, 1))];

  /// An open card, lifted: a white face lit from above (the dark mode's grays), a 1pt accent
  /// rim, two soft shadows.
  List<Color> get liftedCard => brightness == Brightness.light
      ? const [MacosColors.white, Color(0xFFFAFCFF)]
      : const [Color(0xFF2E323C), Color(0xFF252830)];
  List<BoxShadow> get liftedShadow => brightness == Brightness.light
      ? const [
          BoxShadow(color: Color(0x0F0F172A), blurRadius: 2, offset: Offset(0, 1)),
          BoxShadow(color: Color(0x170055FF), blurRadius: 14, offset: Offset(0, 4)),
        ]
      : const [
          BoxShadow(color: Color(0x0F000000), blurRadius: 2, offset: Offset(0, 1)),
          BoxShadow(color: Color(0x45000000), blurRadius: 14, offset: Offset(0, 4)),
        ];
}

/// Motion for the pages that slide over the list.
abstract final class Motion {
  static const quick = Duration(milliseconds: 150);
  static const short = Duration(milliseconds: 200);
  static const medium = Duration(milliseconds: 300);
  static const enter = Duration(milliseconds: 400);
  static const exit = medium;

  /// A strong deceleration: quick off the mark, a long settle.
  static const curve = Cubic(0.05, 0.7, 0.1, 1.0);

  /// Settles a swiped page from wherever the fingers let go, carrying their velocity: critically
  /// damped, so it glides to rest without overshoot.
  static final spring = SpringDescription.withDampingRatio(mass: 1, stiffness: 420, ratio: 1);
}

/// The conversation list's rows: Mail's two-line rows.
abstract final class ListMetrics {
  static const rowHeight = 60.0;
  static const sectionHeight = 30.0;
  static const avatarSize = 36.0;
  static const inset = 6.0;

  /// Lifted rows (selected, under the pointer) stay this far apart, their cards inset by half.
  static const liftGap = 4.0;
}

/// The window as islands, after Dia: the sidebar sits on the window's Liquid Glass, and the list
/// and the conversation are rounded white islands floating on it, [gap] apart and from the
/// window's edges, with [corner] corners. Sheets round at [sheetCorner].
abstract final class Panes {
  static const gap = 8.0;
  static const corner = 12.0;
  static const sheetCorner = 16.0;
}

/// The calendar's events as cards on the tray (MS-163, variant A): [cardRadius] corners and a
/// [cardBorder] hairline, a [dot] of the calendar's color before the title, guests as [avatar]
/// monograms ringed [avatarRing] in the card's color. Overlapping events stand [conflictGap]
/// apart; past ones fade to [pastOpacity], declined ones to [declinedOpacity]; an unanswered
/// invitation is hatched at [hatchOpacity], every [hatchSpacing], inside a border dashed in
/// [dash] lengths. All-day events are bars tinted [allDayTint] with their calendar's color. A
/// day's header carries a [loadBar] tall bar at [loadOpacity] of the accent, as long as its
/// booked hours; the current time runs on across the other days dashed at [nowTrailOpacity].
abstract final class CalendarMetrics {
  static const dot = 6.0;
  static const cardRadius = 8.0;
  static const cardBorder = 0.5;
  static const conflictGap = 2.0;
  static const avatar = 16.0;
  static const avatarRing = 1.5;
  static const loadBar = 2.5;
  static const loadOpacity = 0.55;
  static const pastOpacity = 0.48;
  static const declinedOpacity = 0.40;
  static const hatchOpacity = 0.07;
  static const hatchSpacing = 6.0;
  static const dash = 3.0;
  static const allDayTint = 0.18;
  static const nowTrailOpacity = 0.5;
}

/// A calendar's color softened for its dot, so a screen of events stays quiet.
Color calendarDot(Color calendar, MailColors colors) => Color.lerp(calendar, colors.card, 0.2)!;

/// Guests' monograms on calendar cards: muted indigo, pine, apricot, lavender and slate.
const guestPalette = [
  Color(0xFF6B78B4), Color(0xFF4F8A7B), Color(0xFFC0916A), Color(0xFF9784BE), Color(0xFF7A8290),
];

/// A guest's monogram color, the same for them on every card.
Color guestColor(String key) => guestPalette[stableIndex(key.toLowerCase(), guestPalette.length)];

/// Stable per-account colors, in sidebar order: the system blue, orange, purple, green, red, teal.
const accountPalette = [
  Color(0xFF007AFF), Color(0xFFFF9500), Color(0xFFAF52DE), Color(0xFF34C759), Color(0xFFFF3B30), Color(0xFF30B0C7),
];

/// Stars: the system yellow, deep enough to read on white.
const starColor = Color(0xFFFFB800);

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

/// Sender avatars without a photo: the gray monogram of Contacts and Mail.
({Color background, Color foreground}) avatarColors(String email, Brightness brightness) => brightness == Brightness.light
    ? (background: const Color(0xFFA3A8B3), foreground: MacosColors.white)
    : (background: const Color(0xFF6B6F78), foreground: MacosColors.white);

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
  Brightness get brightness => MacosTheme.of(this).brightness;
  MailColors get colors => brightness == Brightness.light ? MailColors.light : MailColors.dark;
  MacosTypography get text => MacosTheme.of(this).typography;
}
