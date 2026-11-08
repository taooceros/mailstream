import 'package:flutter/foundation.dart' show ValueNotifier, kDebugMode;

import '../model/node.dart';

/// Zero-overhead hook points for DevTools integration.
///
/// `hyper_render_devtools` injects callbacks here at startup so it can
/// observe renderer lifecycle and layout results WITHOUT creating a circular
/// package dependency (core ← devtools would be circular).
///
/// All fields are `null` in release builds and the call sites are guarded by
/// `kDebugMode`, so there is no production overhead.
///
/// ## Usage (by hyper_render_devtools)
/// ```dart
/// HyperRenderDebugHooks.onRendererAttached = (id, getDoc) { ... };
/// HyperRenderDebugHooks.onRendererDetached = (id) { ... };
/// HyperRenderDebugHooks.onLayoutComplete   = (id, getFrags, getLines) { ... };
/// HyperRenderDebugHooks.onFrameTiming      = (id, phase, micros) { ... };
/// HyperRenderDebugHooks.onSelectionChanged = (id, start, end) { ... };
/// HyperRenderDebugHooks.onTextPainterLayout = () { ... };
/// HyperRenderDebugHooks.onLineLayoutTextPainter = () { ... };
/// ```
abstract final class HyperRenderDebugHooks {
  /// Called when a [RenderHyperBox] is attached to the pipeline.
  ///
  /// [id] is stable for the lifetime of this renderer instance.
  /// [getDocument] returns the current [DocumentNode] (may be null if no
  /// content has been set yet).
  static void Function(
    String id,
    DocumentNode? Function() getDocument,
  )? onRendererAttached;

  /// Called when a [RenderHyperBox] is detached from the pipeline.
  static void Function(String id)? onRendererDetached;

  /// Called at the end of each successful [performLayout] pass.
  ///
  /// [getFragments] / [getLines] are lazy getters — call them only if you
  /// actually need the data to avoid unnecessary serialization work.
  static void Function(
    String id,
    List<Map<String, dynamic>> Function() getFragments,
    List<Map<String, dynamic>> Function() getLines,
  )? onLayoutComplete;

  /// Optional getter injected by devtools to retrieve the latest
  /// [PerformanceReport]-like map for a renderer by id.
  ///
  /// Returns null if no performance data is available for [id].
  static Map<String, dynamic>? Function(String id)? getPerformanceData;

  /// Called after each [performLayout] / [paint] of a renderer with its
  /// duration in microseconds. [phase] is `'layout'` or `'paint'`.
  ///
  /// Paint time is canvas *recording* time on the UI thread — it does not
  /// include rasterization (GPU) cost. Each virtualized chunk is its own
  /// renderer, so timings are naturally per chunk.
  static void Function(String id, String phase, int micros)? onFrameTiming;

  /// Called from paint when a renderer's text selection differs from the
  /// last one reported for it. [start]/[end] are the global character
  /// offsets matching each fragment's `globalOffset`; both null when the
  /// selection was cleared.
  static void Function(String id, int? start, int? end)? onSelectionChanged;

  /// Called in debug mode whenever a TextPainter performs layout in RenderHyperBox.
  /// Used by tests and DevTools to count text shaping operations.
  static void Function()? onTextPainterLayout;

  /// Called in debug mode whenever a TextPainter performs layout during the line-breaking
  /// pass in RenderHyperBox.
  static void Function()? onLineLayoutTextPainter;

  /// CSS custom property overrides set from DevTools (`{'--name': value}`).
  ///
  /// In debug builds `HyperViewer` passes these to its
  /// [StyleResolver.customPropertyOverrides] and re-parses whenever the
  /// value changes, so an edited `--var` re-renders live. Applies to every
  /// `HyperViewer` in the app; a bare `HyperRenderWidget` with a prebuilt
  /// document is not re-resolved.
  static final ValueNotifier<Map<String, String>> cssVariableOverrides =
      ValueNotifier(const {});

  /// Returns true only when at least one hook is registered.
  /// Used as a fast-path guard in hot paths.
  static bool get isActive =>
      kDebugMode &&
      (onRendererAttached != null ||
          onRendererDetached != null ||
          onLayoutComplete != null ||
          onFrameTiming != null ||
          onSelectionChanged != null ||
          onTextPainterLayout != null ||
          onLineLayoutTextPainter != null);
}
