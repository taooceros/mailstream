import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:hyper_render/hyper_render.dart';

import 'design.dart';
import 'hyper_resources.dart';
import 'hyper_table.dart';

/// An HTML body drawn by HyperRender, in Flutter. The surrounding Reader owns the sole vertical
/// scroll tree.
class HyperBody extends StatelessWidget {
  const HyperBody({super.key, required this.html});
  final String html;

  @override
  Widget build(BuildContext context) => HyperViewer(
    html: html,
    sanitize: true,
    // Keeps the `data-ms-*` copies of presentational attributes made in preparation.
    allowDataAttributes: true,
    shrinkWrap: true,
    physics: const NeverScrollableScrollPhysics(),
    textColor: const Color(0xff202124),
    customCss: '$mailTableCss body { font-size: ${context.text.body.fontSize ?? 14}px; }',
    widgetBuilder: element,
    imageLoader: loadImage,
    onLinkTap: openLink,
  );

  static void openLink(String url) {
    // The runner checks the link again and refuses what MailLink does not allow.
    if (isMailLink(url)) const MethodChannel('mailstream/window').invokeMethod<void>('openMailLink', url).ignore();
  }

  /// Every element HyperRender lets the host build: tables (and so the tables nested in their
  /// cells, which come back here) and images.
  static Widget? element(UDTNode node) => node is TableNode
      ? MailTable(node: node, onLinkTap: openLink, cellContent: (cell) => HyperRenderWidget(
          document: DocumentNode(children: cell.children),
          selectable: false,
          onLinkTap: openLink,
          widgetBuilder: element,
          imageLoader: loadImage,
        ))
      : imageWidget(node);

  static Widget? imageWidget(UDTNode node) {
    if (node is! AtomicNode || node.tagName != 'img') return null;
    final source = node.src ?? '';
    final uri = Uri.tryParse(source);
    final bytes = inlineImageBytes(source);
    final width = node.intrinsicWidth;
    final height = node.intrinsicHeight;
    Widget unavailable() => Text(node.attributes['alt'] ?? '[Image could not be loaded]', textDirection: TextDirection.ltr);
    if (bytes != null) {
      return Image.memory(bytes, width: width, height: height, fit: BoxFit.contain,
        semanticLabel: node.attributes['alt'], errorBuilder: (_, _, _) => unavailable());
    }
    if (uri?.scheme == 'https' && uri!.host.isNotEmpty && uri.userInfo.isEmpty) {
      return Image.network(source, width: width, height: height, fit: BoxFit.contain,
        semanticLabel: node.attributes['alt'], errorBuilder: (_, _, _) => unavailable());
    }
    return unavailable();
  }

  static void loadImage(String source, void Function(ui.Image) onLoad, void Function(Object) onError) {
    final bytes = inlineImageBytes(source);
    final uri = Uri.tryParse(source);
    final ImageProvider provider;
    if (bytes != null) {
      provider = MemoryImage(bytes);
    } else if (uri?.scheme == 'https' && uri!.host.isNotEmpty && uri.userInfo.isEmpty) {
      provider = NetworkImage(source);
    } else {
      onError(StateError('Unsupported mail image'));
      return;
    }
    final stream = provider.resolve(const ImageConfiguration());
    late final ImageStreamListener listener;
    listener = ImageStreamListener((info, _) {
      stream.removeListener(listener);
      onLoad(info.image.clone());
      info.dispose();
    }, onError: (Object error, StackTrace? stack) {
      stream.removeListener(listener);
      onError(error);
    });
    stream.addListener(listener);
  }
}
