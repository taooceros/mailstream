import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../platform.dart';
import 'design.dart';

/// What draws an HTML body in the reader.
enum ReaderRenderer {
  hyper('HyperRender'),
  blitz('Blitz'),
  webKit('WebView (original)');

  const ReaderRenderer(this.title);
  final String title;

  /// What every launch starts in (MS-166): Blitz, at the user's request.
  static const initial = blitz;

  /// The renderer named by `MAILSTREAM_READER_RENDERER` (`hyper`, `blitz` or `webKit`), so the
  /// bench can open the reader in a given renderer; [initial] when unset or unknown.
  static ReaderRenderer fromName(String? name) =>
      values.firstWhere((r) => r.name.toLowerCase() == name?.toLowerCase(), orElse: () => initial);
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
}

/// The three-way switch in the reader's header, labelled with the renderer in use.
class ReaderRendererSwitch extends StatelessWidget {
  const ReaderRendererSwitch({super.key});

  @override
  Widget build(BuildContext context) {
    final controller = ReaderRendererScope.maybeOf(context);
    if (controller == null || !Host.mac) return const SizedBox.shrink();
    return Row(mainAxisSize: MainAxisSize.min, children: [
      Text('Renderer:', style: context.text.body),
      const SizedBox(width: 6),
      MacosPopupButton<ReaderRenderer>(
        value: controller.value,
        onChanged: (renderer) {
          if (renderer != null) controller.value = renderer;
        },
        items: [for (final r in ReaderRenderer.values) MacosPopupMenuItem(value: r, child: Text(r.title))],
      ),
    ]);
  }
}
