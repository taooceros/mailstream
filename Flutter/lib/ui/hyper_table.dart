import 'dart:math' as math;

import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';
import 'package:hyper_render/hyper_render.dart';

/// Mail-client defaults HyperRender's own user-agent sheet gets wrong for mail: it puts a 16 px
/// margin and a border on every table and 12 px padding and a border on every cell, where a
/// browser has none, and lets `text-align` run into nested tables, which a browser's quirks mode
/// (mail without a doctype) resets at each table. Passed as `customCss`, so the mail's own styles
/// still win. Also maps the `align` and `valign` attributes (as preparation copied them to
/// `data-ms-*`) to CSS.
final mailTableCss = [
  'table { margin: 0; border: 0; text-align: left; } td, th { padding: 0; border: 0; } '
      'thead, th { background-color: transparent; }',
  for (final value in ['left', 'center', 'right'])
    '${_selectors('align', value)} { text-align: $value; }',
  for (final value in ['top', 'middle', 'bottom'])
    '${_selectors('valign', value)} { vertical-align: $value; }',
].join(' ');

String _selectors(String attribute, String value) =>
    ['td', 'th', 'tr', 'div', 'p'].map((tag) => '$tag[data-ms-$attribute="$value"]').join(', ');

/// A mail `<table>` on HyperRender's table layout, read the way mail clients read it: grid lines
/// only for `border="1"` tables, `cellpadding` or the cell's own CSS padding, `bgcolor`, cell and
/// row heights, cells as tall as their row, `align`, and widths that fit the reading column
/// (`width="600"` is at most 600, never a horizontal scroller that clips the text), instead of
/// HyperRender's default of a 1 px grid and 8 px padding around every cell of every table and
/// natural (unwrapped) widths for tables without a percentage width.
class MailTable extends StatelessWidget {
  const MailTable({super.key, required this.node, required this.cellContent, required this.onLinkTap});

  final TableNode node;

  /// Renders a cell's children; tables inside them come back here through the widget builder.
  final Widget Function(TableCellNode cell) cellContent;

  /// Links in cells holding only inline content, which HyperTable renders itself.
  final void Function(String url) onLinkTap;

  static const gridColor = Color(0xff9e9e9e);

  @override
  Widget build(BuildContext context) {
    final style = node.style;
    final widthAttribute = node.attributes['width']?.trim();
    // A CSS width wins over the `width` attribute.
    final css = style.width != null || style.widthPercent != null;
    final percent = css ? style.widthPercent : _percent(widthAttribute);
    final pixels = css ? style.width : _pixels(widthAttribute);
    final maxWidth = [pixels, style.maxWidth].whereType<double>().fold<double?>(null, (a, b) => a == null ? b : math.min(a, b));
    final cellPadding = _pixels(node.attributes['data-ms-cellpadding']) ?? 1;
    final border = _pixels(node.attributes['data-ms-border']) ?? (node.attributes.containsKey('data-ms-border') ? 1 : 0);
    final nested = context.findAncestorWidgetOfExactType<MailTable>() != null;
    final cellAlign = context.dependOnInheritedWidgetOfExactType<_CellAlign>()?.value;

    Widget table = HyperTable(
      tableNode: node,
      onLinkTap: onLinkTap,
      // One-pixel lines between all cells is the closest this layout gets to `border="N"`.
      borderWidth: border > 0 ? 1 : 0,
      borderColor: gridColor,
      cellPadding: EdgeInsets.zero,
      // One selection area per top-level table, as HyperRender's own default does.
      selectable: !nested,
      fillWidth: percent != null || pixels != null,
      cellBuilder: (cell, row, content) => _cell(cell, row, content, cellPadding),
      // Cells holding blocks (paragraphs, nested tables, images); HyperTable keeps its own
      // selectable text for cells holding only inline content.
      cellContentBuilder: cellContent,
    );
    final background = _visible(style.backgroundColor) ?? _color(node.attributes['data-ms-bgcolor']);
    final outline = _border(style);
    if (background != null || outline != null || style.borderRadius != null) {
      table = DecoratedBox(
        decoration: BoxDecoration(color: background, border: outline, borderRadius: style.borderRadius),
        child: Padding(padding: outline?.dimensions ?? EdgeInsets.zero, child: table),
      );
    }
    if (style.padding != EdgeInsets.zero) table = Padding(padding: style.padding, child: table);
    if (maxWidth != null) table = ConstrainedBox(constraints: BoxConstraints(maxWidth: maxWidth), child: table);
    final align = switch (_align(cellAlign)) {
      'center' => Alignment.topCenter,
      'right' => Alignment.topRight,
      _ => null,
    };
    if (percent != null && percent < 1) {
      // The Align takes the full column, so the fraction applies once even when HyperRender lays
      // the table out again at the width it measured.
      final placement = align ?? Alignment.topLeft;
      return Align(alignment: placement, child: FractionallySizedBox(widthFactor: percent, alignment: placement, child: table));
    }
    return align == null ? table : Align(alignment: align, widthFactor: null, child: table);
  }

  /// The `align` that places this table: its own, else the nearest enclosing element's within the
  /// cell (or body) holding it, else the enclosing cell's. Browsers centre a table inside
  /// `<td align="center">` or `<div align="center">`, though CSS `text-align` alone does not.
  String? _align(String? cellAlign) {
    for (UDTNode? at = node; at != null; at = at.parent) {
      final value = at.attributes['data-ms-align'];
      if (value != null) return value;
    }
    return cellAlign;
  }

  Widget _cell(TableCellNode cell, TableRowNode row, Widget content, double cellPadding) {
    final style = cell.style;
    final height = style.height ?? row.style.height ?? _pixels(cell.attributes['height']) ?? _pixels(row.attributes['height']);
    final vertical = style.verticalAlign != HyperVerticalAlign.baseline ? style.verticalAlign : row.style.verticalAlign;
    final (x, textAlign) = switch (style.textAlign) {
      HyperTextAlign.center => (0.0, TextAlign.center),
      HyperTextAlign.right => (1.0, TextAlign.right),
      HyperTextAlign.justify => (-1.0, TextAlign.justify),
      HyperTextAlign.left => (-1.0, TextAlign.left),
    };
    Widget slot = Container(
      padding: style.padding != EdgeInsets.zero ? style.padding : EdgeInsets.all(cellPadding),
      constraints: height == null ? null : BoxConstraints(minHeight: height),
      // The table stretches every cell to its row, so its content is placed here.
      alignment: Alignment(x, switch (vertical) {
        HyperVerticalAlign.middle => 0,
        HyperVerticalAlign.bottom || HyperVerticalAlign.textBottom => 1,
        _ => -1,
      }),
      decoration: BoxDecoration(
        color: _visible(style.backgroundColor) ?? _visible(row.style.backgroundColor) ??
            _color(cell.attributes['data-ms-bgcolor']) ?? _color(row.attributes['data-ms-bgcolor']),
        border: _border(style),
      ),
      child: cell.children.isEmpty ? null : _CellAlign(
        value: cell.attributes['data-ms-align'] ?? row.attributes['data-ms-align'],
        // HyperTable's own text for inline-only cells follows the cell's `text-align`.
        child: DefaultTextStyle.merge(textAlign: textAlign, child: content),
      ),
    );
    final width = style.width ?? _pixels(cell.attributes['width']);
    if (width != null) slot = _PreferredWidth(width: width, child: slot);
    return slot;
  }

  static Border? _border(ComputedStyle style) {
    final color = style.borderColor;
    final width = style.borderWidth;
    if (color == null || color.a == 0 || width == EdgeInsets.zero) return null;
    BorderSide side(double w) => w > 0 ? BorderSide(color: color, width: w) : BorderSide.none;
    return Border(top: side(width.top), right: side(width.right), bottom: side(width.bottom), left: side(width.left));
  }

  static Color? _visible(Color? color) => color == null || color.a == 0 ? null : color;

  static double? _percent(String? value) {
    if (value == null || !value.endsWith('%')) return null;
    final parsed = double.tryParse(value.substring(0, value.length - 1).trim());
    return parsed == null || parsed <= 0 ? null : math.min(parsed, 100) / 100;
  }

  static double? _pixels(String? value) {
    if (value == null) return null;
    final match = RegExp(r'^\s*(\d+(?:\.\d+)?)\s*(?:px)?\s*$').firstMatch(value);
    return match == null ? null : double.parse(match.group(1)!);
  }

  static final _hex = RegExp(r'^#?([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$');
  static const _named = {'white': Color(0xffffffff), 'black': Color(0xff000000)};

  static Color? _color(String? value) {
    if (value == null) return null;
    final trimmed = value.trim().toLowerCase();
    final named = _named[trimmed];
    if (named != null) return named;
    var hex = _hex.firstMatch(trimmed)?.group(1);
    if (hex == null) return null;
    if (hex.length == 3) hex = hex.split('').map((c) => '$c$c').join();
    return Color(0xff000000 | int.parse(hex, radix: 16));
  }
}

/// The `align` of the cell (or its row) holding a table: a cell's children are rendered apart
/// from the cell's node, so a table there cannot find it by walking its parents. Each cell sets
/// its own, so it never reaches the tables of a nested table's cells (quirks mode stops there).
class _CellAlign extends InheritedWidget {
  const _CellAlign({required this.value, required super.child});

  final String? value;

  @override
  bool updateShouldNotify(_CellAlign oldWidget) => value != oldWidget.value;
}

/// A cell with a declared width, as CSS's automatic table layout treats it: the column wants that
/// width (its max-content becomes max(min-content, width)) but may still shrink to its content's
/// minimum when the reading column is narrower. An empty spacer cell keeps its width that way.
class _PreferredWidth extends SingleChildRenderObjectWidget {
  const _PreferredWidth({required this.width, required super.child});

  final double width;

  @override
  _RenderPreferredWidth createRenderObject(BuildContext context) => _RenderPreferredWidth(width);

  @override
  void updateRenderObject(BuildContext context, _RenderPreferredWidth renderObject) => renderObject.width = width;
}

class _RenderPreferredWidth extends RenderProxyBox {
  _RenderPreferredWidth(this._width);

  double _width;
  set width(double value) {
    if (value == _width) return;
    _width = value;
    markNeedsLayout();
  }

  @override
  double computeMaxIntrinsicWidth(double height) => math.max(super.computeMinIntrinsicWidth(height), _width);
}
