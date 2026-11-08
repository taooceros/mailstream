import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'dart:math' as math;

import 'package:flutter/widgets.dart';
import 'package:webview_plus/webview_plus.dart';

import 'html_parts.dart';

/// One HTML body on Windows, drawn by WebView2 through `webview_plus` into a Flutter texture, as
/// tall as its content, so it scrolls with the conversation (the Mac's is `MailHtmlView` in the
/// runner). The mail's own scripts never run (`javaScriptEnabled: false` is WebView2's
/// `IsScriptEnabled`); the app's measuring script runs through `ExecuteScript`, which that switch
/// does not stop and the mail cannot reach. Links open in the default browser. Remote images load
/// in an InPrivate profile, without cookies.
///
/// The page is a file in a temporary folder, not a string: WebView2 ignores a base URL for
/// strings, so the `file://` inline parts in Mailstream's cache (`cid:` images) would not load,
/// and it caps strings at 2 MB. The file goes with the view.
class WindowsMailHtml extends StatefulWidget {
  const WindowsMailHtml({super.key, required this.html, required this.onOpenLink});

  final String html;

  /// Opens a link the reader clicked, in the browser (or mail app for `mailto:`).
  final ValueChanged<String> onOpenLink;

  @override
  State<WindowsMailHtml> createState() => _WindowsMailHtmlState();
}

class _WindowsMailHtmlState extends State<WindowsMailHtml> {
  /// The page file, once written.
  String? _path;

  WebviewPlusController? _controller;
  bool _loaded = false;

  /// Until the page measures itself: about two lines.
  double _height = 40;

  /// The width the last measurement ran at; another width lays the page out again.
  double? _measuredWidth;

  /// Measurements after load, while late images and fonts can still change the height.
  final List<Timer> _later = [];

  static const _settings = WebviewSettings(
    javaScriptEnabled: false,
    incognito: true,
    cacheEnabled: false,
    domStorageEnabled: false,
    thirdPartyCookiesEnabled: false,
    initialBackgroundColor: Color(0xFFFFFFFF),
    hideNativeScrollbars: true,
    disableLinkHoverPreview: false,
    supportZoom: false,
    builtInZoomControls: false,
    interceptedResourceExtensions: [],
    mediaPlaybackRequiresUserGesture: true,
  );

  @override
  void initState() {
    super.initState();
    _write();
  }

  Future<void> _write() async {
    final String path;
    try {
      path = await _writePage(widget.html);
    } catch (error, stack) {
      FlutterError.reportError(FlutterErrorDetails(exception: error, stack: stack, library: 'mail HTML'));
      return;
    }
    if (!mounted) {
      _delete(path);
      return;
    }
    setState(() => _path = path);
  }

  static int _next = 0;

  /// Writes [html]'s page ([windowsMailPage]) in a background isolate. Static, so the closure
  /// sent to the isolate holds only strings, never this state.
  static Future<String> _writePage(String html) async {
    final folder = await _folder;
    final path = '$folder${Platform.pathSeparator}$pid-${_next++}.html';
    await Isolate.run(() => File(path).writeAsStringSync(windowsMailPage(html)), debugName: 'mail-html-page');
    return path;
  }

  /// The pages' folder, emptied once per run of what an earlier run left behind.
  static final Future<String> _folder = () async {
    final folder = Directory('${Directory.systemTemp.path}${Platform.pathSeparator}mailstream-html');
    if (await folder.exists()) await folder.delete(recursive: true).catchError((Object _) => folder);
    await folder.create(recursive: true);
    return folder.path;
  }();

  static void _delete(String path) => File(path).delete().catchError((Object _) => File(path));

  @override
  void dispose() {
    for (final timer in _later) {
      timer.cancel();
    }
    final path = _path;
    if (path != null) _delete(path);
    super.dispose();
  }

  void _onLoad() {
    _loaded = true;
    _measureAt(const [0, 300, 1000, 3000]);
  }

  /// Measures after each delay, in place of the measurements still waiting. WebView2 takes its new
  /// width a moment after Flutter lays the view out (`setSize` goes over the plugin's channel), so
  /// a width change measures more than once.
  void _measureAt(List<int> delays) {
    for (final timer in _later) {
      timer.cancel();
    }
    _later
      ..clear()
      ..addAll([for (final delay in delays) Timer(Duration(milliseconds: delay), _measure)]);
  }

  /// The texture holds the whole page: WebView2 composes no taller than Direct3D's 16384 px
  /// texture limit, so a longer body stops there and scrolls inside.
  double get _tallest => 16384 / MediaQuery.devicePixelRatioOf(context) - 1;

  Future<void> _measure() async {
    final controller = _controller;
    final box = context.findRenderObject();
    if (!mounted || controller == null || !_loaded || box is! RenderBox || !box.hasSize) return;
    _measuredWidth = box.size.width;
    final Object? answer;
    try {
      answer = await controller.evaluateJavascript(_script(_tallest));
    } on Object {
      return; // The view can go before it answers.
    }
    if (!mounted) return;
    final size = answer is String ? jsonDecode(answer) : answer;
    if (size is! Map || size['height'] is! num) return;
    final height = math.min((size['height'] as num).toDouble().ceilToDouble(), _tallest);
    if ((height - _height).abs() >= 1) setState(() => _height = height);
  }

  Future<bool> _navigate(WebviewPlusController _, Uri uri) async {
    // The page itself never navigates. Web and mail links go to the browser or mail app; other
    // schemes (protocol handlers the shell would launch, `javascript:`) are dropped.
    if (const {'http', 'https', 'mailto'}.contains(uri.scheme)) widget.onOpenLink(uri.toString());
    return false;
  }

  @override
  Widget build(BuildContext context) {
    final path = _path;
    return LayoutBuilder(builder: (context, constraints) {
      final width = constraints.maxWidth;
      if (_loaded && _measuredWidth != null && (width - _measuredWidth!).abs() >= 0.5) {
        _measuredWidth = width;
        _measureAt(const [50, 250, 600]);
      }
      return SizedBox(
        height: _height,
        child: path == null
            ? null
            : WebviewWidget(
                initialFile: path,
                initialSettings: _settings,
                onWebViewCreated: (controller) => _controller = controller,
                onLoadStop: (_, _) => _onLoad(),
                onNavigationRequest: _navigate,
              ),
      );
    });
  }

  /// Applied over the mail's own styles, as on the Mac (`MailHtmlView.script`): the document sizes
  /// to its content and nothing sizes from the viewport (a quirks-mode `height: 100%` would grow
  /// with the view without end). Content wider than the view zooms out to fit, down to 30%.
  /// Answers the height in logical pixels as JSON; past [tallest] the page scrolls inside.
  static String _script(double tallest) => '''
(() => {
  const doc = document, root = doc.documentElement, body = doc.body;
  if (!doc.getElementById('mailstream-style')) {
    const style = doc.createElement('style');
    style.id = 'mailstream-style';
    style.textContent = `
      html, body { height: auto !important; min-height: 0 !important; overflow: hidden !important; }
      html { display: flex !important; flex-direction: column !important; }
      body { margin: 0; padding: 2px 0; color: #1f1f1f; font: 14px/1.45 "Segoe UI", system-ui, sans-serif;
        overflow-wrap: break-word; }
      img { max-width: 100%; }
      pre { white-space: pre-wrap; }
    `;
    (doc.head || root).appendChild(style);
  }
  const target = body || root;
  target.style.zoom = '';
  const room = root.clientWidth;
  const width = Math.max(root.scrollWidth, body ? body.scrollWidth : 0);
  if (room > 0 && width > room + 2) target.style.zoom = Math.max(room / width, 0.3);
  const box = target.getBoundingClientRect();
  const margin = body ? parseFloat(getComputedStyle(body).marginBottom) || 0 : 0;
  const height = box.bottom + window.scrollY + margin;
  const overflow = height > $tallest ? 'auto' : 'hidden';
  for (const element of [root, body]) if (element) element.style.setProperty('overflow', overflow, 'important');
  return JSON.stringify({ height });
})()
''';
}
