import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:macos_ui/macos_ui.dart';

/// The Mac's look: macos_ui's controls (push buttons, icon buttons, search and text fields,
/// switches, pop-up buttons, spinners) under [macos], and Material's remaining widgets themed to
/// match by [of]: the system font at the Mac's type sizes, the accent blue on neutral grays
/// (Material's token names, Apple's values), no ink ripples. Only what a theme cannot carry lives
/// in the other classes here: motion, the list's row heights, the islands' metrics, stable
/// account and topic colors.
abstract final class MailTheme {
  static const accent = Color(0xFF007AFF);
  static const accentDark = Color(0xFF0A84FF);

  /// The Mac's semantic colors: labels, fills, separators and backgrounds, light and dark.
  static ColorScheme scheme(Brightness brightness) => brightness == Brightness.light
      ? const ColorScheme(
          brightness: Brightness.light,
          primary: accent,
          onPrimary: Colors.white,
          primaryContainer: Color(0xFFE3EFFF),
          onPrimaryContainer: Color(0xFF0040A0),
          secondary: Color(0xFF6E6E73),
          onSecondary: Colors.white,
          // Selected rows and chips: the unemphasized selection gray.
          secondaryContainer: Color(0xFFE4E4E8),
          onSecondaryContainer: Color(0xFF1D1D1F),
          tertiary: Color(0xFF5856D6),
          onTertiary: Colors.white,
          tertiaryContainer: Color(0xFFECEBFB),
          onTertiaryContainer: Color(0xFF3634A3),
          error: Color(0xFFFF3B30),
          onError: Colors.white,
          errorContainer: Color(0xFFFFE8E6),
          onErrorContainer: Color(0xFFB3261E),
          surface: Colors.white,
          onSurface: Color(0xFF1D1D1F),
          onSurfaceVariant: Color(0xFF6E6E73),
          surfaceDim: Color(0xFFE5E5EA),
          surfaceBright: Colors.white,
          surfaceContainerLowest: Colors.white,
          surfaceContainerLow: Color(0xFFF7F7F9),
          surfaceContainer: Color(0xFFF2F2F5),
          surfaceContainerHigh: Color(0xFFECECF0),
          surfaceContainerHighest: Color(0xFFE5E5EA),
          outline: Color(0xFFC7C7CC),
          outlineVariant: Color(0xFFE5E5EA),
          shadow: Colors.black,
          scrim: Colors.black,
          inverseSurface: Color(0xFF2C2C2E),
          onInverseSurface: Color(0xFFF5F5F7),
          inversePrimary: accentDark,
          surfaceTint: Colors.transparent,
        )
      : const ColorScheme(
          brightness: Brightness.dark,
          primary: accentDark,
          onPrimary: Colors.white,
          primaryContainer: Color(0xFF0F2D52),
          onPrimaryContainer: Color(0xFFB8D8FF),
          secondary: Color(0xFF98989D),
          onSecondary: Colors.black,
          secondaryContainer: Color(0xFF3A3A3C),
          onSecondaryContainer: Color(0xFFF5F5F7),
          tertiary: Color(0xFF7D7AFF),
          onTertiary: Colors.white,
          tertiaryContainer: Color(0xFF26254D),
          onTertiaryContainer: Color(0xFFD0CFFF),
          error: Color(0xFFFF453A),
          onError: Colors.white,
          errorContainer: Color(0xFF4D1D1A),
          onErrorContainer: Color(0xFFFFB4AB),
          surface: Color(0xFF1E1E1E),
          onSurface: Color(0xFFF5F5F7),
          onSurfaceVariant: Color(0xFF98989D),
          surfaceDim: Color(0xFF161616),
          surfaceBright: Color(0xFF3A3A3C),
          surfaceContainerLowest: Color(0xFF161616),
          surfaceContainerLow: Color(0xFF252527),
          surfaceContainer: Color(0xFF2A2A2C),
          surfaceContainerHigh: Color(0xFF323234),
          surfaceContainerHighest: Color(0xFF3A3A3C),
          outline: Color(0xFF545458),
          outlineVariant: Color(0xFF38383A),
          shadow: Colors.black,
          scrim: Colors.black,
          inverseSurface: Color(0xFFE5E5EA),
          onInverseSurface: Color(0xFF1D1D1F),
          inversePrimary: accent,
          surfaceTint: Colors.transparent,
        );

  /// The Mac's type sizes (13 pt body, 11 pt small print, 20–26 pt titles) on the system font
  /// Flutter already uses on macOS; no Material letter spacing.
  static const _text = TextTheme(
    displayLarge: TextStyle(fontSize: 34, fontWeight: FontWeight.w700, letterSpacing: -0.4, height: 1.2),
    displayMedium: TextStyle(fontSize: 28, fontWeight: FontWeight.w700, letterSpacing: -0.4, height: 1.2),
    displaySmall: TextStyle(fontSize: 26, fontWeight: FontWeight.w700, letterSpacing: -0.3, height: 1.2),
    headlineLarge: TextStyle(fontSize: 26, fontWeight: FontWeight.w700, letterSpacing: -0.3, height: 1.2),
    headlineMedium: TextStyle(fontSize: 22, fontWeight: FontWeight.w700, letterSpacing: -0.3, height: 1.25),
    headlineSmall: TextStyle(fontSize: 20, fontWeight: FontWeight.w600, letterSpacing: -0.2, height: 1.25),
    titleLarge: TextStyle(fontSize: 20, fontWeight: FontWeight.w700, letterSpacing: -0.2, height: 1.25),
    titleMedium: TextStyle(fontSize: 15, fontWeight: FontWeight.w600, letterSpacing: 0, height: 1.3),
    titleSmall: TextStyle(fontSize: 13, fontWeight: FontWeight.w600, letterSpacing: 0, height: 1.3),
    bodyLarge: TextStyle(fontSize: 14, fontWeight: FontWeight.w400, letterSpacing: 0, height: 1.4),
    bodyMedium: TextStyle(fontSize: 13, fontWeight: FontWeight.w400, letterSpacing: 0, height: 1.35),
    bodySmall: TextStyle(fontSize: 11, fontWeight: FontWeight.w400, letterSpacing: 0, height: 1.3),
    labelLarge: TextStyle(fontSize: 13, fontWeight: FontWeight.w500, letterSpacing: 0, height: 1.3),
    labelMedium: TextStyle(fontSize: 12, fontWeight: FontWeight.w500, letterSpacing: 0, height: 1.3),
    labelSmall: TextStyle(fontSize: 11, fontWeight: FontWeight.w500, letterSpacing: 0, height: 1.3),
  );

  /// macos_ui's theme, in the same accent.
  static MacosThemeData macos(Brightness brightness) => brightness == Brightness.light
      ? MacosThemeData.light(accentColor: AccentColor.blue)
      : MacosThemeData.dark(accentColor: AccentColor.blue);

  static ThemeData of(Brightness brightness) {
    final scheme = MailTheme.scheme(brightness);
    final light = brightness == Brightness.light;
    final hover = scheme.onSurface.withValues(alpha: light ? 0.06 : 0.08);
    final pressed = scheme.onSurface.withValues(alpha: light ? 0.12 : 0.16);
    WidgetStateProperty<Color?> overlay(Color base) => WidgetStateProperty.resolveWith((states) {
      if (states.contains(WidgetState.pressed)) return base.withValues(alpha: 0.16);
      if (states.contains(WidgetState.hovered) || states.contains(WidgetState.focused)) return base.withValues(alpha: 0.08);
      return null;
    });
    const capsule = StadiumBorder();
    const buttonText = TextStyle(fontSize: 13, fontWeight: FontWeight.w500, letterSpacing: 0);
    const buttonPadding = EdgeInsets.symmetric(horizontal: 14);
    const buttonSize = Size(0, 30);
    return ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      textTheme: _text,
      visualDensity: VisualDensity.compact,
      materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
      splashFactory: NoSplash.splashFactory,
      hoverColor: hover,
      highlightColor: pressed,
      focusColor: hover,
      scaffoldBackgroundColor: Colors.transparent,
      canvasColor: scheme.surface,
      dividerTheme: DividerThemeData(color: scheme.outlineVariant, thickness: 0.5, space: 1),
      iconTheme: IconThemeData(size: 17, color: scheme.onSurfaceVariant),
      iconButtonTheme: IconButtonThemeData(
        style: ButtonStyle(
          iconSize: const WidgetStatePropertyAll(17),
          minimumSize: const WidgetStatePropertyAll(Size(30, 30)),
          fixedSize: const WidgetStatePropertyAll(Size(30, 30)),
          padding: const WidgetStatePropertyAll(EdgeInsets.zero),
          shape: WidgetStatePropertyAll(RoundedRectangleBorder(borderRadius: BorderRadius.circular(8))),
          foregroundColor: WidgetStateProperty.resolveWith(
            (states) => states.contains(WidgetState.disabled) ? scheme.onSurfaceVariant.withValues(alpha: 0.4) : scheme.onSurfaceVariant,
          ),
          overlayColor: overlay(scheme.onSurface),
          splashFactory: NoSplash.splashFactory,
        ),
      ),
      filledButtonTheme: FilledButtonThemeData(
        style: ButtonStyle(
          shape: const WidgetStatePropertyAll(capsule),
          minimumSize: const WidgetStatePropertyAll(buttonSize),
          padding: const WidgetStatePropertyAll(buttonPadding),
          textStyle: const WidgetStatePropertyAll(buttonText),
          elevation: const WidgetStatePropertyAll(0),
          iconSize: const WidgetStatePropertyAll(15),
          overlayColor: overlay(Colors.white),
          splashFactory: NoSplash.splashFactory,
        ),
      ),
      textButtonTheme: TextButtonThemeData(
        style: ButtonStyle(
          shape: const WidgetStatePropertyAll(capsule),
          minimumSize: const WidgetStatePropertyAll(buttonSize),
          padding: const WidgetStatePropertyAll(buttonPadding),
          textStyle: const WidgetStatePropertyAll(buttonText),
          iconSize: const WidgetStatePropertyAll(15),
          overlayColor: overlay(scheme.primary),
          splashFactory: NoSplash.splashFactory,
        ),
      ),
      // The Mac's secondary push button: a gray capsule, no outline.
      outlinedButtonTheme: OutlinedButtonThemeData(
        style: ButtonStyle(
          shape: const WidgetStatePropertyAll(capsule),
          side: const WidgetStatePropertyAll(BorderSide.none),
          backgroundColor: WidgetStatePropertyAll(scheme.onSurface.withValues(alpha: light ? 0.06 : 0.12)),
          foregroundColor: WidgetStatePropertyAll(scheme.onSurface),
          minimumSize: const WidgetStatePropertyAll(buttonSize),
          padding: const WidgetStatePropertyAll(buttonPadding),
          textStyle: const WidgetStatePropertyAll(buttonText),
          iconSize: const WidgetStatePropertyAll(15),
          overlayColor: overlay(scheme.onSurface),
          splashFactory: NoSplash.splashFactory,
        ),
      ),
      inputDecorationTheme: InputDecorationTheme(
        isDense: true,
        filled: true,
        fillColor: light ? Colors.white : scheme.surfaceContainerHigh,
        contentPadding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        // A rounded box with a hairline side and the Mac's focus ring. Filled fields take their side
        // from `activeIndicatorBorder`; only `border` is set, so `InputBorder.none` stays bare.
        border: OutlineInputBorder(borderRadius: BorderRadius.circular(8)),
        activeIndicatorBorder: WidgetStateBorderSide.resolveWith((states) => states.contains(WidgetState.focused)
            ? BorderSide(color: scheme.primary.withValues(alpha: 0.7), width: 2.5)
            : BorderSide(color: scheme.outline.withValues(alpha: 0.6), width: 0.5)),
      ),
      chipTheme: ChipThemeData(
        side: BorderSide.none,
        backgroundColor: scheme.onSurface.withValues(alpha: light ? 0.05 : 0.1),
        labelStyle: TextStyle(fontSize: 12, fontWeight: FontWeight.w500, color: scheme.onSurfaceVariant),
        padding: const EdgeInsets.symmetric(horizontal: 2),
        shape: const StadiumBorder(),
      ),
      cardTheme: CardThemeData(
        elevation: 0,
        margin: EdgeInsets.zero,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Panes.corner)),
      ),
      dialogTheme: DialogThemeData(
        backgroundColor: scheme.surface,
        elevation: 24,
        shadowColor: Colors.black.withValues(alpha: 0.4),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Panes.sheetCorner)),
        titleTextStyle: _text.titleMedium?.copyWith(color: scheme.onSurface),
        contentTextStyle: _text.bodyMedium?.copyWith(color: scheme.onSurface),
      ),
      listTileTheme: ListTileThemeData(
        dense: true,
        iconColor: scheme.onSurfaceVariant,
        titleTextStyle: _text.bodyMedium?.copyWith(color: scheme.onSurface),
        subtitleTextStyle: _text.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
      ),
      switchTheme: SwitchThemeData(
        trackColor: WidgetStateProperty.resolveWith((states) =>
            states.contains(WidgetState.selected) ? scheme.primary : scheme.onSurface.withValues(alpha: 0.12)),
        thumbColor: const WidgetStatePropertyAll(Colors.white),
        trackOutlineColor: const WidgetStatePropertyAll(Colors.transparent),
        thumbIcon: const WidgetStatePropertyAll(null),
      ),
      progressIndicatorTheme: ProgressIndicatorThemeData(
        color: scheme.primary,
        linearTrackColor: Colors.transparent,
        linearMinHeight: 2,
      ),
      scrollbarTheme: ScrollbarThemeData(
        thickness: const WidgetStatePropertyAll(6),
        radius: const Radius.circular(3),
        thumbColor: WidgetStatePropertyAll(scheme.onSurface.withValues(alpha: 0.35)),
        crossAxisMargin: 2,
      ),
      popupMenuTheme: PopupMenuThemeData(
        color: light ? const Color(0xFFF6F6F6) : const Color(0xFF2C2C2E),
        elevation: 12,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(10)),
        textStyle: _text.bodyMedium?.copyWith(color: scheme.onSurface),
      ),
      menuTheme: MenuThemeData(
        style: MenuStyle(
          backgroundColor: WidgetStatePropertyAll(light ? const Color(0xFFF6F6F6) : const Color(0xFF2C2C2E)),
          shape: WidgetStatePropertyAll(RoundedRectangleBorder(borderRadius: BorderRadius.circular(10))),
        ),
      ),
      navigationRailTheme: NavigationRailThemeData(
        backgroundColor: Colors.transparent,
        indicatorColor: scheme.onSurface.withValues(alpha: light ? 0.08 : 0.14),
        indicatorShape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
        selectedIconTheme: IconThemeData(color: scheme.primary, size: 19),
        unselectedIconTheme: IconThemeData(color: scheme.onSurfaceVariant, size: 19),
        selectedLabelTextStyle: _text.labelSmall?.copyWith(color: scheme.onSurface),
        unselectedLabelTextStyle: _text.labelSmall?.copyWith(color: scheme.onSurfaceVariant),
      ),
      badgeTheme: BadgeThemeData(backgroundColor: scheme.error, textStyle: _text.labelSmall),
      // The Mac's floating notices are dark rounded panels, not Material's bar.
      snackBarTheme: SnackBarThemeData(
        behavior: SnackBarBehavior.floating,
        width: 460,
        elevation: 8,
        backgroundColor: scheme.inverseSurface,
        contentTextStyle: _text.bodyMedium?.copyWith(color: scheme.onInverseSurface),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
      ),
      // The Mac's tooltips: small print on a light box with a hairline.
      tooltipTheme: TooltipThemeData(
        waitDuration: const Duration(milliseconds: 600),
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 3),
        textStyle: _text.bodySmall?.copyWith(color: scheme.onSurface),
        decoration: BoxDecoration(
          color: light ? const Color(0xFFF6F6F6) : const Color(0xFF2C2C2E),
          borderRadius: BorderRadius.circular(5),
          border: Border.all(color: scheme.outline.withValues(alpha: 0.5), width: 0.5),
          boxShadow: [BoxShadow(color: Colors.black.withValues(alpha: 0.12), blurRadius: 6, offset: const Offset(0, 2))],
        ),
      ),
    );
  }
}

/// Motion for the pages that slide over the list.
abstract final class Motion {
  static const enter = Durations.medium4;
  static const exit = Durations.medium2;
  static const curve = Easing.emphasizedDecelerate;

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
}

/// The window as islands, after Dia: the sidebar sits on the window's Liquid Glass, and the list
/// and the conversation are rounded white islands floating on it, [gap] apart and from the
/// window's edges, with [corner] corners. Sheets round at [sheetCorner].
abstract final class Panes {
  static const gap = 8.0;
  static const corner = 12.0;
  static const sheetCorner = 16.0;
}

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
    ? (background: const Color(0xFFA3A8B3), foreground: Colors.white)
    : (background: const Color(0xFF6B6F78), foreground: Colors.white);

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
