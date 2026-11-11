import 'dart:convert';

import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../platform.dart';

/// The app's look, the same on the Mac and on Windows (MS-182): every color, radius, space,
/// elevation, icon size and text role the UI paints with is a token here, and the controls draw
/// themselves from them rather than from either system's look (system glass, vibrancy, platform
/// controls). What differs by platform is only the window chrome, the shortcuts and the fonts the
/// type falls back to ([MailFonts]). [MailColors] (`context.colors`) carries the color roles and
/// the elevations, [Radii], [Space], [Strokes] and [IconSizes] the scales, [TextRoles]
/// (`context.roles`) the type, [Motion] the timing; use the nearest step of a scale, and add a
/// token rather than a literal. macos_ui's
/// theme ([MailTheme]) still styles the controls not yet replaced by the app's own (MS-193), and
/// its typography (`context.text`) the text not yet moved to [TextRoles].
abstract final class MailTheme {
  static MacosThemeData macos(Brightness brightness) => brightness == Brightness.light
      ? MacosThemeData.light(accentColor: AccentColor.blue)
      : MacosThemeData.dark(accentColor: AccentColor.blue);
}

/// The color roles, light and dark: labels, fills, separators, surfaces and the accents, after
/// Apple's system colors on both platforms.
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
    required this.capsule,
    required this.capsuleRim,
    required this.avatarPalette,
  });

  static const light = MailColors._(
    brightness: Brightness.light,
    window: Color(0xFFE4E5EA),
    background: Color(0xFFFFFFFF),
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
    card: Color(0xFFFFFFFF),
    cardBorder: Color(0x14000000),
    raisedCard: Color(0xFFFFFFFF),
    spotlight: Color(0xA6FFFFFF),
    capsule: Color(0xFFF5F5F7),
    capsuleRim: Color(0x14000000),
    avatarPalette: [
      Color(0xFF007AFF), Color(0xFFAF52DE), Color(0xFFFF2D55), Color(0xFFFF3B30), Color(0xFFFF9500), Color(0xFF34C759),
      Color(0xFF30B0C7), Color(0xFF5856D6), Color(0xFFA2845E), Color(0xFF00C7BE), Color(0xFF32ADE6),
    ],
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
    capsule: Color(0xFF2C2C2E),
    capsuleRim: Color(0x1FFFFFFF),
    avatarPalette: [
      Color(0xFF0A84FF), Color(0xFFBF5AF2), Color(0xFFFF375F), Color(0xFFFF453A), Color(0xFFFF9F0A), Color(0xFF30D158),
      Color(0xFF40C8E0), Color(0xFF5E5CE6), Color(0xFFAC8E68), Color(0xFF63E6E2), Color(0xFF64D2FF),
    ],
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

  /// The face of a toolbar capsule floating over content, opaque, inside a [capsuleRim]
  /// hairline.
  final Color capsule;
  final Color capsuleRim;

  /// People's colors where there is no picture, after the system hues (blue, purple, pink, red,
  /// orange, green, teal, indigo, brown, mint, cyan) the web reader's avatars use; [avatarHue]
  /// picks one per person.
  final List<Color> avatarPalette;

  /// Links in text the app draws, and the default for links mail leaves uncolored.
  Color get link => accent;

  /// [key]'s color from [avatarPalette], the same on every launch (a lowercased address, else a
  /// Chat user id).
  Color avatarHue(String key) => avatarPalette[stableIndex(key.toLowerCase(), avatarPalette.length)];

  /// Hover and press washes over the sidebar, buttons and menu items; rows and cards lift instead
  /// ([halfLifted]).
  Color get hover => label.withValues(alpha: brightness == Brightness.light ? 0.05 : 0.08);
  Color get pressed => label.withValues(alpha: brightness == Brightness.light ? 0.1 : 0.14);

  // Elevation, lowest first: a resting card ([cardShadow]), a raised control or a card half lifted
  // under the pointer ([raisedShadow]), a lifted card ([liftedShadow]), an island on the window
  // ([islandShadow]), a popover, menu or toast ([popoverShadow]), a sheet ([sheetShadow]).

  /// A resting [card]'s shadow, one soft layer.
  List<BoxShadow> get cardShadow => brightness == Brightness.light
      ? const [BoxShadow(color: Color(0x0F0F172A), blurRadius: 3, offset: Offset(0, 1))]
      : const [BoxShadow(color: Color(0x33000000), blurRadius: 3, offset: Offset(0, 1))];

  /// A control raised off its surface (a selected segment, a capsule over a row, today's date), and
  /// a row or card half lifted under the pointer.
  List<BoxShadow> get raisedShadow => brightness == Brightness.light
      ? const [
          BoxShadow(color: Color(0x14000000), blurRadius: 2, offset: Offset(0, 1)),
          BoxShadow(color: Color(0x14000000), blurRadius: 10, offset: Offset(0, 4)),
        ]
      : const [
          BoxShadow(color: Color(0x66000000), blurRadius: 2, offset: Offset(0, 1)),
          BoxShadow(color: Color(0x4D000000), blurRadius: 10, offset: Offset(0, 4)),
        ];

  /// The one lifted-card recipe (MS-189), for whatever the user opened or selected on a tray:
  /// list rows ([LiftedCard], with its accent bar) and the calendar's events alike. A face lit
  /// from above ([liftedCard], white or one dark gray), a [Strokes.rim] of [liftedRim], the
  /// [liftedShadow]. Under the pointer or pressed, a thing lifts half as far: a [halfLifted] face
  /// where it has none of its own, a [cardBorder] hairline and the [raisedShadow].
  List<Color> get liftedCard => brightness == Brightness.light
      ? const [Color(0xFFFFFFFF), Color(0xFFFAFCFF)]
      : const [Color(0xFF2C2C30), Color(0xFF252529)];
  Color get liftedRim => brightness == Brightness.light ? const Color(0x52007AFF) : const Color(0x4D0A84FF);
  /// Opaque, so the [raisedShadow] under it doesn't show through: the tray brightened most of the
  /// way to white, one step over it in dark mode.
  Color get halfLifted => brightness == Brightness.light ? const Color(0xFFFCFCFE) : const Color(0xFF2A2A2C);
  List<BoxShadow> get liftedShadow => brightness == Brightness.light
      ? const [
          BoxShadow(color: Color(0x0F0F172A), blurRadius: 2, offset: Offset(0, 1)),
          BoxShadow(color: Color(0x170055FF), blurRadius: 14, offset: Offset(0, 4)),
        ]
      : const [
          BoxShadow(color: Color(0x0F000000), blurRadius: 2, offset: Offset(0, 1)),
          BoxShadow(color: Color(0x45000000), blurRadius: 14, offset: Offset(0, 4)),
        ];

  /// The list and the conversation floating on the window.
  List<BoxShadow> get islandShadow => brightness == Brightness.light
      ? const [
          BoxShadow(color: Color(0x0D000000), blurRadius: 1, offset: Offset(0, 0.5)),
          BoxShadow(color: Color(0x12000000), blurRadius: 18, offset: Offset(0, 4)),
        ]
      : const [
          BoxShadow(color: Color(0x4D000000), blurRadius: 1, offset: Offset(0, 0.5)),
          BoxShadow(color: Color(0x59000000), blurRadius: 18, offset: Offset(0, 4)),
        ];

  /// Popovers, menus and toasts over the window.
  List<BoxShadow> get popoverShadow => const [BoxShadow(color: Color(0x33000000), blurRadius: 18, offset: Offset(0, 6))];

  /// A sheet over the dimmed window.
  List<BoxShadow> get sheetShadow => const [BoxShadow(color: Color(0x47000000), blurRadius: 40, offset: Offset(0, 16))];
}

/// Corner radii: [small] for inline marks, [control] for buttons, rows and event cards, [card]
/// for cards, banners and tiles, [pane] for the islands, [panel] for the composer and large
/// panels, [sheet] for sheets; capsules are fully round. The same steps as the native app's
/// `Design.Radius`.
abstract final class Radii {
  static const small = 4.0;
  static const control = 8.0;
  static const card = 10.0;
  static const pane = 12.0;
  static const panel = 14.0;
  static const sheet = 16.0;
}

/// The spacing scale for padding and gaps, in points.
abstract final class Space {
  static const s2 = 2.0;
  static const s4 = 4.0;
  static const s6 = 6.0;
  static const s8 = 8.0;
  static const s10 = 10.0;
  static const s12 = 12.0;
  static const s16 = 16.0;
  static const s20 = 20.0;
  static const s24 = 24.0;
  static const s32 = 32.0;
}

/// Line widths: a [hairline] for separators, card and capsule rims; a [rim] for the accent edge
/// of a lifted or focused thing.
abstract final class Strokes {
  static const hairline = 0.5;
  static const rim = 1.0;
}

/// Icon sizes: [inline] beside small print, [row] in rows and chips, [toolbar] on toolbar
/// buttons, [hero] for the large glyphs of empty states and send buttons.
abstract final class IconSizes {
  static const inline = 12.0;
  static const row = 15.0;
  static const toolbar = 17.0;
  static const hero = 24.0;
}

/// The type's families: the system's UI font on the Mac, Segoe UI on Windows, each falling back
/// to the platform's Chinese UI font; on Linux the desktop's own (fontconfig's `sans-serif` and
/// `monospace`), which fontconfig falls back from per character. [mono] sets code.
abstract final class MailFonts {
  static String get family => Host.mac
      ? '.AppleSystemUIFont'
      : Host.linux
          ? 'sans-serif'
          : 'Segoe UI Variable Text';
  static List<String> get fallback => Host.mac
      ? const ['PingFang SC']
      : Host.linux
          ? const []
          : const ['Segoe UI', 'Microsoft YaHei UI'];
  static String get mono => Host.linux ? 'monospace' : 'Menlo';
}

/// The text roles, in the label color unless a role says otherwise: [display] titles a detail
/// (a subject, an event), [paneTitle] a list or panel, [headline] names a sender or a group,
/// [body] is the UI's text, [reading] message text in the reader and chat, [meta] small print
/// in the secondary color, [tag] the label of a tag.
final class TextRoles {
  const TextRoles._(this._colors);

  final MailColors _colors;

  TextStyle _role(double size, FontWeight weight, {double? height, Color? color}) => TextStyle(
    fontFamily: MailFonts.family,
    fontFamilyFallback: MailFonts.fallback,
    fontSize: size,
    fontWeight: weight,
    height: height,
    letterSpacing: 0,
    color: color ?? _colors.label,
  );

  TextStyle get display => _role(22, FontWeight.w700);
  TextStyle get paneTitle => _role(15, FontWeight.w700);
  TextStyle get headline => _role(13, FontWeight.w600);
  TextStyle get body => _role(13, FontWeight.w400);
  TextStyle get reading => _role(14, FontWeight.w400, height: 1.5);
  TextStyle get meta => _role(11, FontWeight.w400, color: _colors.secondaryLabel);
  TextStyle get tag => _role(10, FontWeight.w600);
}

/// Motion for the pages that slide over the list, the calendar's pages and views.
abstract final class Motion {
  static const quick = Duration(milliseconds: 150);

  /// A day or week sliding in after ← or →.
  static const page = Duration(milliseconds: 180);
  static const short = Duration(milliseconds: 200);

  /// One open item giving way to the next beside the list (an event after ↑ or ↓).
  static const swap = Duration(milliseconds: 240);
  static const medium = Duration(milliseconds: 300);
  static const enter = Duration(milliseconds: 400);
  static const exit = medium;

  /// Where a view or pane coming in starts its scale, growing to full size as it fades in.
  static const enterScale = 0.985;

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

  /// A selected row's accent bar at its card's start: [liftBar] wide, [liftBarInset] from the
  /// card's top and bottom.
  static const liftBar = 3.0;
  static const liftBarInset = 14.0;
}

/// The window as islands, after Dia: the sidebar sits on the window's Liquid Glass, and the list
/// and the conversation are rounded white islands floating on it, [gap] apart and from the
/// window's edges, with [corner] corners. Sheets round at [sheetCorner].
abstract final class Panes {
  static const gap = Space.s8;
  static const corner = Radii.pane;
  static const sheetCorner = Radii.sheet;
}

/// The calendar's events as cards on the tray (MS-163, variant A): [cardRadius] corners and a
/// [cardBorder] hairline, a [MarkMetrics.dot] of the calendar's color before the title, guests as
/// [avatar] [PersonAvatar]s ringed in the card's color. Overlapping events stand
/// [conflictGap] apart; past ones fade to [pastOpacity], declined ones to [declinedOpacity]; an
/// unanswered invitation is hatched at [hatchOpacity], every [hatchSpacing], inside a border
/// dashed in [dash] lengths. All-day events are bars tinted [allDayTint] with their calendar's
/// color. A day's header carries a [loadBar] tall bar at [loadOpacity] of the accent, as long as
/// its booked hours; the current time runs on across the other days dashed at [nowTrailOpacity].
abstract final class CalendarMetrics {
  static const cardRadius = Radii.control;
  static const cardBorder = Strokes.hairline;
  static const conflictGap = 2.0;
  static const avatar = 16.0;
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

/// Marks (MS-190): an account, a calendar or a topic is a [dot] of its color, the one shape for
/// them everywhere; unread is an [unread] accent dot in an [unreadSlot] wide column before the
/// avatar, in every list.
abstract final class MarkMetrics {
  static const dot = 6.0;
  static const unread = 7.0;
  static const unreadSlot = 16.0;
}

/// Avatars (MS-192), the web reader's look everywhere: a person's picture, else their initial in
/// their [MailColors.avatarHue] at [letter] of the avatar's size (a group's glyph at [glyph]), on
/// a [tint] wash of that hue. Where avatars overlap (a card's guests) each is ringed [ring] wide
/// in the surface under it.
abstract final class AvatarMetrics {
  static const tint = 0.15;
  static const letter = 0.4;
  static const glyph = 0.5;
  static const ring = 1.5;
}

/// Tags (MS-190): small print in its color on a [tint] wash of it, fully round, [height] tall or
/// [compactHeight] on cards and in dense rows, [padding] at either end.
abstract final class TagMetrics {
  static const height = 18.0;
  static const compactHeight = 16.0;
  static const tint = 0.12;
  static const padding = Space.s6;
}

/// Notices (MS-191, `MailNotice`): a wash of the tone's color, [tint] of it in light mode and
/// [darkTint] in dark, where the low share keeps the hue quiet so a notice never outweighs the
/// page, inside a [rim] hairline of it; [compactHeight] tall as one line over a view.
abstract final class NoticeMetrics {
  static const tint = 0.10;
  static const darkTint = 0.14;
  static const rim = 0.22;
  static const compactHeight = 28.0;
}

/// A calendar's color softened for its dot, so a screen of events stays quiet.
Color calendarDot(Color calendar, MailColors colors) => Color.lerp(calendar, colors.card, 0.2)!;

/// Stable per-account colors, in sidebar order: the system blue, orange, purple, green, red, teal.
const accountPalette = [
  Color(0xFF007AFF), Color(0xFFFF9500), Color(0xFFAF52DE), Color(0xFF34C759), Color(0xFFFF3B30), Color(0xFF30B0C7),
];

/// The color of the account at [index] in the sidebar's order (`MailState.accountIndex`).
Color accountColor(int index) => accountPalette[index % accountPalette.length];

/// Stars: the system yellow, deep enough to read on white.
const starColor = Color(0xFFFFB800);

/// What a file is, for the color of its glyph.
enum AttachmentKind { image, video, audio, pdf, document, file }

/// Attachments' colors by kind (MS-191), light and dark, after the system hues the rest of the
/// app uses: pictures purple, videos pink, audio orange, PDFs red, documents (a Drive file)
/// blue, any other file gray.
const _attachmentHues = <AttachmentKind, (Color, Color)>{
  AttachmentKind.image: (Color(0xFFAF52DE), Color(0xFFBF5AF2)),
  AttachmentKind.video: (Color(0xFFFF2D55), Color(0xFFFF375F)),
  AttachmentKind.audio: (Color(0xFFFF9500), Color(0xFFFF9F0A)),
  AttachmentKind.pdf: (Color(0xFFFF3B30), Color(0xFFFF453A)),
  AttachmentKind.document: (Color(0xFF007AFF), Color(0xFF0A84FF)),
  AttachmentKind.file: (Color(0xFF8E8E93), Color(0xFF98989D)),
};

Color attachmentColor(AttachmentKind kind, Brightness brightness) {
  final pair = _attachmentHues[kind]!;
  return brightness == Brightness.dark ? pair.$2 : pair.$1;
}

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
  TextRoles get roles => TextRoles._(colors);
}
