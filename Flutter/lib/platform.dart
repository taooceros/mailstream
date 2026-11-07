import 'dart:io' show Platform;

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart' show SingleActivator;

/// The desktop whose conventions the app follows. On the Mac: ⌘ shortcuts, the runner's AppKit
/// glass, window chrome and WebKit bodies (`macos/Runner`). Elsewhere (Windows): Ctrl shortcuts,
/// capsules Flutter draws, the system title bar and the Windows runner's channels
/// (`windows/runner`).
abstract final class Host {
  /// Set by tests to take the other desktop's branch on any host; `null` follows the host.
  @visibleForTesting
  static bool? debugMacOverride;

  /// Whether the app runs on the Mac.
  static bool get mac => debugMacOverride ?? Platform.isMacOS;

  /// Whether the command modifier is down: ⌘ on the Mac, Ctrl elsewhere. Ctrl with Alt is AltGr
  /// on Windows, which types characters (`[` on many layouts) rather than commands.
  static bool get commandPressed {
    final keyboard = HardwareKeyboard.instance;
    return mac ? keyboard.isMetaPressed : keyboard.isControlPressed && !keyboard.isAltPressed;
  }

  /// [key] with the command modifier, and Shift with [shift].
  static SingleActivator command(LogicalKeyboardKey key, {bool shift = false}) =>
      SingleActivator(key, meta: mac, control: !mac, shift: shift);

  /// How the command shortcut of [key] (named as the Mac names it: `W`, `Return`, `,`) reads in a
  /// tooltip: `⌘W`, `⌘⇧O`, `⌘ Return` on the Mac; `Ctrl+W`, `Ctrl+Shift+O`, `Ctrl+Enter` elsewhere.
  static String shortcut(String key, {bool shift = false}) {
    if (mac) return '⌘${shift ? '⇧' : ''}${key.length > 1 ? ' ' : ''}$key';
    return 'Ctrl+${shift ? 'Shift+' : ''}${key == 'Return' ? 'Enter' : key}';
  }

  /// The delete key's name: ⌫ on the Mac, Del elsewhere (Backspace works too).
  static String get deleteKey => mac ? '⌫' : 'Del';
}
