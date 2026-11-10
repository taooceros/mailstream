import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/gestures.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:mailstream/mailstream.dart' as engine;

import '../platform.dart';
import 'mail_resources.dart';

/// An HTML body drawn by Blitz (MS-147) in the engine's library (`FFI/src/html_page.rs`): parsing,
/// layout and drawing run on a thread of their own there, and this widget only shows the tiles it
/// draws, strips [tileHeight] points tall, for the part of the body in or near the reader's
/// viewport. The body is as tall as its content and scrolls with the conversation, as the WebView's
/// does. A click opens the link under it ([openMailLink], under the WebView's rules), a mouse drag
/// selects text and ⌘C (Ctrl+C) copies it.
class BlitzBody extends StatefulWidget {
  const BlitzBody({super.key, required this.html, this.page = _page, this.openLink = BlitzBody.openMailLink});

  final String html;

  /// Makes the page for [html]; tests answer from memory.
  final engine.HtmlPageInterface Function(String html) page;
  final void Function(String url) openLink;

  static const tileHeight = 512.0;

  static engine.HtmlPageInterface _page(String html) => engine.HtmlPage(html: html);

  /// Opens [url] through the runner's `MailLink`, which checks it again and refuses what it does
  /// not allow.
  static void openMailLink(String url) {
    if (isMailLink(url)) const MethodChannel('mailstream/window').invokeMethod<void>('openMailLink', url).ignore();
  }

  /// Tiles this far (in tiles) beyond the viewport are drawn ahead; past twice this they are freed.
  static const ahead = 1;

  @override
  State<BlitzBody> createState() => _BlitzBodyState();
}

class _Tile {
  _Tile(this.image, this.top, this.height, this.generation);

  final ui.Image image;

  /// Points, in the layout it was drawn for.
  final double top;
  final double height;
  final int generation;
}

class _BlitzBodyState extends State<BlitzBody> {
  late engine.HtmlPageInterface _page;
  bool _closed = false;

  /// Until the page is laid out: about two lines, as the WebView's.
  double _height = 40;

  /// The width and scale the page is (being) laid out for, and the version of its content.
  double? _width;
  double? _scale;
  int _version = 0;

  /// Grows whenever drawn tiles go stale (another layout, late content, a selection); a stale tile
  /// stays on screen until its replacement arrives, so nothing flashes.
  int _generation = 0;
  final _tiles = <int, _Tile>{};

  /// The generation each tile being drawn was asked for in.
  final _drawing = <int, int>{};
  ScrollPosition? _position;

  MouseCursor _cursor = MouseCursor.defer;
  Offset? _hovered;
  bool _hovering = false;
  Offset? _dragStart;
  (Offset, Offset)? _pendingSelection;
  bool _selecting = false;
  bool _selected = false;
  final _focus = FocusNode(debugLabel: 'mail HTML (Blitz)');
  String? _text;

  @override
  void initState() {
    super.initState();
    _open();
  }

  void _open() {
    _page = widget.page(widget.html);
    _watch(_page);
  }

  /// Late content (a remote image) changes the page: lay it out and draw it again.
  Future<void> _watch(engine.HtmlPageInterface page) async {
    while (!_closed && identical(page, _page)) {
      final version = await page.changed(version: _version);
      if (version == null || _closed || !identical(page, _page)) return;
      _version = version;
      _layOut(force: true);
    }
  }

  @override
  void didUpdateWidget(BlitzBody old) {
    super.didUpdateWidget(old);
    if (old.html == widget.html) return;
    _page.close();
    _clearTiles();
    _width = _scale = null;
    _version = 0;
    _selected = false;
    _text = null;
    _open();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final position = Scrollable.maybeOf(context)?.position;
    if (position == _position) return;
    _position?.removeListener(_updateTiles);
    _position = position?..addListener(_updateTiles);
  }

  @override
  void dispose() {
    _closed = true;
    _page.close();
    _position?.removeListener(_updateTiles);
    _clearTiles();
    _focus.dispose();
    super.dispose();
  }

  void _clearTiles() {
    for (final tile in _tiles.values) {
      tile.image.dispose();
    }
    _tiles.clear();
    _drawing.clear();
  }

  Future<void> _layOut({bool force = false, double? width, double? scale}) async {
    width ??= _width;
    scale ??= _scale;
    if (width == null || scale == null) return;
    if (!force && width == _width && scale == _scale) return;
    _width = width;
    _scale = scale;
    final page = _page;
    final size = await page.layout(width: width, scale: scale);
    if (size == null || !mounted || !identical(page, _page) || width != _width || scale != _scale) return;
    _generation++;
    setState(() => _height = size.height);
    // Drawn once this layout is on screen, where the viewport says which tiles show.
    WidgetsBinding.instance.addPostFrameCallback((_) => _updateTiles());
  }

  /// The body's part in the scroll view's viewport, in its own coordinates; all of it without one.
  (double, double)? _visible() {
    final box = context.findRenderObject();
    if (box is! RenderBox || !box.attached || !box.hasSize) return null;
    final viewport = Scrollable.maybeOf(context)?.context.findRenderObject();
    if (viewport is! RenderBox || !viewport.hasSize) return (0, box.size.height);
    final top = box.globalToLocal(viewport.localToGlobal(Offset.zero)).dy;
    return (top, top + viewport.size.height);
  }

  /// Draws the tiles near the viewport that are missing or stale and frees those far from it.
  void _updateTiles() {
    if (_closed || _width == null) return;
    final visible = _visible();
    if (visible == null) return;
    const tile = BlitzBody.tileHeight;
    final count = (_height / tile).ceil();
    int index(double y) => (y / tile).floor().clamp(0, math.max(0, count - 1));
    if (visible.$2 < -tile * BlitzBody.ahead || visible.$1 > _height + tile * BlitzBody.ahead) {
      // Far from the viewport (another message's turn): keep nothing.
      _evict(0, -1);
      return;
    }
    final first = index(visible.$1 - tile * BlitzBody.ahead);
    final last = index(visible.$2 + tile * BlitzBody.ahead);
    _evict(index(visible.$1 - tile * 2 * BlitzBody.ahead), index(visible.$2 + tile * 2 * BlitzBody.ahead));
    for (var i = first; i <= last; i++) {
      if (_tiles[i]?.generation == _generation || _drawing[i] == _generation) continue;
      _draw(i);
    }
  }

  void _evict(int first, int last) {
    final gone = _tiles.keys.where((i) => i < first || i > last).toList();
    if (gone.isEmpty) return;
    setState(() {
      for (final i in gone) {
        _tiles.remove(i)!.image.dispose();
      }
    });
  }

  Future<void> _draw(int i) async {
    final (page, generation, width, scale) = (_page, _generation, _width!, _scale!);
    final top = i * BlitzBody.tileHeight;
    final height = math.min(BlitzBody.tileHeight, _height - top);
    if (height <= 0) return;
    _drawing[i] = generation;
    final tile = await page.tile(width: width, scale: scale, top: top, height: height);
    if (tile == null || _closed || !identical(page, _page)) return;
    final completer = Completer<ui.Image>();
    ui.decodeImageFromPixels(tile.pixels, tile.width, tile.height, ui.PixelFormat.rgba8888, completer.complete);
    final image = await completer.future;
    if (_drawing[i] == generation) _drawing.remove(i);
    final current = _tiles[i];
    if (_closed || !identical(page, _page) || (current != null && current.generation >= generation)) {
      image.dispose();
      return;
    }
    setState(() => _tiles[i] = _Tile(image, top, height, generation));
    current?.image.dispose();
  }

  void _hover(PointerHoverEvent event) {
    _hovered = event.localPosition;
    if (!_hovering) _hoverLink();
  }

  /// One question in flight: the last position asked about wins.
  Future<void> _hoverLink() async {
    final at = _hovered;
    if (at == null) return;
    _hovering = true;
    final link = await _page.linkAt(x: at.dx, y: at.dy);
    _hovering = false;
    if (_closed) return;
    final cursor = link != null ? SystemMouseCursors.click : MouseCursor.defer;
    if (cursor != _cursor) setState(() => _cursor = cursor);
    if (_hovered != at) _hoverLink();
  }

  Future<void> _tap(TapUpDetails details) async {
    if (_selected) {
      _selected = false;
      if (await _page.clearSelection()) _redraw();
    }
    final link = await _page.linkAt(x: details.localPosition.dx, y: details.localPosition.dy);
    if (link != null && !_closed) widget.openLink(link);
  }

  void _dragStarted(DragStartDetails details) {
    _dragStart = details.localPosition;
    _focus.requestFocus();
    _select(details.localPosition);
  }

  void _dragged(DragUpdateDetails details) => _select(details.localPosition);

  /// One selection in flight: the drag's latest point wins.
  Future<void> _select(Offset to) async {
    final from = _dragStart;
    if (from == null) return;
    _pendingSelection = (from, to);
    if (_selecting) return;
    _selecting = true;
    while (!_closed) {
      final pending = _pendingSelection;
      if (pending == null) break;
      _pendingSelection = null;
      _selected = await _page.select(fromX: pending.$1.dx, fromY: pending.$1.dy, toX: pending.$2.dx, toY: pending.$2.dy);
      if (!_closed) _redraw();
    }
    _selecting = false;
  }

  /// Draws the tiles again (a selection changed): the shown ones stay until replaced.
  void _redraw() {
    _generation++;
    _updateTiles();
  }

  KeyEventResult _key(FocusNode node, KeyEvent event) {
    final keyboard = HardwareKeyboard.instance;
    final command = Host.mac ? keyboard.isMetaPressed : keyboard.isControlPressed;
    if (event is! KeyDownEvent || !command || event.logicalKey != LogicalKeyboardKey.keyC || !_selected) {
      return KeyEventResult.ignored;
    }
    _page.selectedText().then((text) {
      if (text != null) Clipboard.setData(ClipboardData(text: text));
    });
    return KeyEventResult.handled;
  }

  void _readText() {
    if (_text != null) return;
    _text = '';
    _page.text().then((text) {
      if (mounted && text != null) setState(() => _text = text);
    });
  }

  @override
  Widget build(BuildContext context) {
    final scale = MediaQuery.devicePixelRatioOf(context);
    if (SemanticsBinding.instance.semanticsEnabled) _readText();
    return LayoutBuilder(builder: (context, constraints) {
      final width = constraints.maxWidth;
      if (width != _width || scale != _scale) {
        // After this build: a layout asks the engine, which answers later.
        Future.microtask(() => _layOut(width: width, scale: scale));
      }
      return Semantics(
        container: true,
        label: _text,
        child: Focus(
          focusNode: _focus,
          onKeyEvent: _key,
          child: MouseRegion(
            cursor: _cursor,
            onHover: _hover,
            onExit: (_) => _hovered = null,
            child: RawGestureDetector(
              behavior: HitTestBehavior.opaque,
              gestures: {
                TapGestureRecognizer: GestureRecognizerFactoryWithHandlers<TapGestureRecognizer>(
                  TapGestureRecognizer.new,
                  (recognizer) => recognizer.onTapUp = _tap,
                ),
                PanGestureRecognizer: GestureRecognizerFactoryWithHandlers<PanGestureRecognizer>(
                  () => PanGestureRecognizer(supportedDevices: {PointerDeviceKind.mouse}),
                  (recognizer) => recognizer
                    // The selection starts where the button went down, not where the drag won.
                    ..dragStartBehavior = DragStartBehavior.down
                    ..onStart = _dragStarted
                    ..onUpdate = _dragged
                    ..onEnd = (_) => _dragStart = null,
                ),
              },
              child: SizedBox(
                height: _height,
                width: double.infinity,
                child: CustomPaint(painter: _TilesPainter(_tiles.values.toList(), _width ?? width)),
              ),
            ),
          ),
        ),
      );
    });
  }
}

class _TilesPainter extends CustomPainter {
  _TilesPainter(this.tiles, this.width);

  final List<_Tile> tiles;
  final double width;

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()..filterQuality = FilterQuality.low;
    for (final tile in tiles) {
      final image = tile.image;
      final source = Rect.fromLTWH(0, 0, image.width.toDouble(), image.height.toDouble());
      canvas.drawImageRect(image, source, Rect.fromLTWH(0, tile.top, width, tile.height), paint);
    }
  }

  @override
  bool shouldRepaint(_TilesPainter old) =>
      old.width != width || old.tiles.length != tiles.length || Iterable.generate(tiles.length).any((i) => !identical(old.tiles[i], tiles[i]));
}
