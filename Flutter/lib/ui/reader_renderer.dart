import 'package:flutter/widgets.dart';

import '../platform.dart';
import 'controls.dart';
import 'design.dart';

/// What draws an HTML body in the reader, in the switch's order.
enum ReaderRenderer {
  blitz('Blitz'),
  webKit('WebView (original)');

  const ReaderRenderer(this.title);
  final String title;

  /// What every launch starts in (MS-166): Blitz, at the user's request.
  static const initial = blitz;

  /// The renderer named by `MAILSTREAM_READER_RENDERER` (`blitz` or `webKit`), so the bench can
  /// open the reader in a given renderer; [initial] when unset or unknown (the removed `hyper`
  /// included, MS-196).
  static ReaderRenderer fromName(String? name) =>
      values.firstWhere((r) => r.name.toLowerCase() == name?.toLowerCase(), orElse: () => initial);

  /// Whether bodies it draws need the reader's prepared parts (`HtmlPartsCache`): the Mac's
  /// WebKit loads the mail as the engine gave it and folds quotes in the page (MS-167); Windows'
  /// WebView2 draws the split parts whatever the switch says.
  bool get preparesHtml => !Host.mac || this != webKit;
}

/// The Mac reader's renderer, kept in memory: every launch starts in Blitz, and a choice
/// made in the reader holds until the app quits. Nothing switches it but the user (no automatic
/// fallback), and it touches no MailState, cache or setting. Windows always draws with WebView2.
class ReaderRendererScope extends InheritedNotifier<ValueNotifier<ReaderRenderer>> {
  const ReaderRendererScope({super.key, required ValueNotifier<ReaderRenderer> controller, required super.child})
      : super(notifier: controller);

  static ValueNotifier<ReaderRenderer>? maybeOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<ReaderRendererScope>()?.notifier;

  static ReaderRenderer current(BuildContext context) => maybeOf(context)?.value ?? ReaderRenderer.initial;

  /// [current] for a callback: [context] does not rebuild when the renderer changes.
  static ReaderRenderer read(BuildContext context) =>
      context.getInheritedWidgetOfExactType<ReaderRendererScope>()?.notifier?.value ?? ReaderRenderer.initial;
}

/// The two-way switch in the reader's header, labelled with the renderer in use.
class ReaderRendererSwitch extends StatelessWidget {
  const ReaderRendererSwitch({super.key});

  @override
  Widget build(BuildContext context) {
    final controller = ReaderRendererScope.maybeOf(context);
    if (controller == null || !Host.mac) return const SizedBox.shrink();
    return Row(mainAxisSize: MainAxisSize.min, children: [
      Text('Renderer:', style: context.text.body),
      const SizedBox(width: Space.s6),
      MailMenuButton<ReaderRenderer>(
        value: controller.value,
        size: MailControlSize.small,
        onChanged: (renderer) => controller.value = renderer,
        items: [for (final r in ReaderRenderer.values) MailMenuItem(r, r.title)],
      ),
    ]);
  }
}
