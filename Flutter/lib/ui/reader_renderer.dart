import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';

import '../platform.dart';
import 'design.dart';

/// What draws an HTML body in the reader.
enum ReaderRenderer {
  hyper('HyperRender'),
  webKit('WebView (original)');

  const ReaderRenderer(this.title);
  final String title;
}

/// The Mac reader's renderer, kept in memory: every launch starts in HyperRender, and a choice
/// made in the reader holds until the app quits. Nothing switches it but the user (no automatic
/// fallback), and it touches no MailState, cache or setting. Windows always draws with WebView2.
class ReaderRendererScope extends InheritedNotifier<ValueNotifier<ReaderRenderer>> {
  const ReaderRendererScope({super.key, required ValueNotifier<ReaderRenderer> controller, required super.child})
      : super(notifier: controller);

  static ValueNotifier<ReaderRenderer>? maybeOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<ReaderRendererScope>()?.notifier;

  static ReaderRenderer current(BuildContext context) => maybeOf(context)?.value ?? ReaderRenderer.hyper;
}

/// The switch in the reader's header, labelled with the renderer in use.
class ReaderRendererSwitch extends StatelessWidget {
  const ReaderRendererSwitch({super.key});

  @override
  Widget build(BuildContext context) {
    final controller = ReaderRendererScope.maybeOf(context);
    if (controller == null || !Host.mac) return const SizedBox.shrink();
    final renderer = controller.value;
    return Row(mainAxisSize: MainAxisSize.min, children: [
      MacosSwitch(
        value: renderer == ReaderRenderer.hyper,
        onChanged: (hyper) => controller.value = hyper ? ReaderRenderer.hyper : ReaderRenderer.webKit,
      ),
      const SizedBox(width: 8),
      Text('Renderer: ${renderer.title}', style: context.text.body),
    ]);
  }
}
