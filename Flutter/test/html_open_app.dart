import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream/mailstream.dart' as engine;
import 'package:mailstream_ui/platform.dart';
import 'package:mailstream_ui/ui/blitz_body.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/reader.dart';
import 'package:mailstream_ui/ui/reader_renderer.dart';

// How long an HTML body takes to open (MS-167, MS-234): from mounting the reader's `MailHtml` to
// the first height its page reports (laid out, its height in Flutter), for four synthetic mails
// one at a time and all four at once (a thread opening), in the renderer
// `MAILSTREAM_READER_RENDERER` names (`blitz` or `webKit`, the default). Each opening has HTML of
// its own, so no idle page serves it. Synthetic mail only: no accounts or private mail; the
// inline image is a synthetic PNG in a folder of its own under the inline cache, removed at the
// end.
// flutter build macos --release -t test/html_open_app.dart
// MAILSTREAM_HTML_OPEN_OUT=/tmp/open.json MAILSTREAM_READER_RENDERER=blitz "build/macos/Build/Products/Release/Mailstream Flutter.app/Contents/MacOS/Mailstream Flutter"
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Host.mac) await WindowManipulator.initialize(enableWindowDelegate: true);
  runApp(const HtmlOpenApp());
}

/// A synthetic message folder: hex, like the engine's keys.
const _key = 'f167f167f167f167f167f167f167f167';
const _pixel = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
// 1×1 PNG.
const _png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';

final _inlineRoot = engine.inlinePartsFolder();
String get _inline => 'file://$_inlineRoot/$_key/0-logo.png';

String _quote(int paragraphs) => '<div class="gmail_quote"><div dir="ltr" class="gmail_attr">On Mon, Nov 9, 2026 at 9:00 AM '
    'Alex &lt;alex@example.test&gt; wrote:<br></div><blockquote class="gmail_quote" style="margin:0 0 0 .8ex;'
    'border-left:1px #ccc solid;padding-left:1ex">${'<p>Earlier synthetic message, folded behind the toggle.</p>' * paragraphs}'
    '</blockquote></div>';
String _note(int run) => '<html><head><style>p{margin:0 0 8px}</style></head><body><div dir="ltr">'
    '${'<p>Thanks for the notes. The synthetic draft looks right; one question about the second section below.</p>' * 24}'
    '<img src="$_inline" width="120" height="40" alt="logo"><!-- run $run --></div>${_quote(12)}</body></html>';

String _row(int i) => '<tr><td align="left" valign="top" bgcolor="#ffffff" style="padding:10px 24px;'
    'font:15px Helvetica,Arial,sans-serif;color:#202020;border-bottom:1px solid #eeeeee">'
    '<a href="https://example.test/item/$i" style="color:#1a6dd6;text-decoration:none">Synthetic item $i</a>'
    '<br>A line of description for item $i, as a digest or newsletter writes it.</td>'
    '<td align="right" valign="top" width="120" style="padding:10px 24px"><img src="$_pixel" width="96" height="64" '
    'style="display:block;background:#dde7f0" alt=""></td></tr>';

String _digest(int run) => '<!DOCTYPE html><html><head><meta charset="utf-8"></head><body style="margin:0">'
    '<table width="100%" border="0" cellpadding="0" cellspacing="0">${List.generate(36, _row).join()}</table>'
    '<!-- run $run -->${_quote(10)}</body></html>';

String _newsletter(int run, int rows, int images) => '<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" '
    '"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head>'
    '<style type="text/css">body,#bodyTable{height:100% !important;margin:0;padding:0;width:100% !important}'
    '@media only screen and (max-width:600px){.column{display:block!important;width:100%!important}}</style></head>'
    '<body><center><table align="center" border="0" cellpadding="0" cellspacing="0" height="100%" width="100%" '
    'id="bodyTable" bgcolor="#eeeeee"><tr><td align="center" valign="top" style="padding:10px">'
    '<table border="0" cellpadding="0" cellspacing="0" width="600" bgcolor="#ffffff">'
    '${List.generate(images, (_) => '<tr><td align="center"><img src="$_inline" width="600" height="120" style="display:block"></td></tr>').join()}'
    '${List.generate(rows, _row).join()}</table></td></tr></table><!-- run $run --></center></body></html>';

/// Name, then the HTML of its opening number `run`.
final _mails = <(String, String Function(int))>[
  ('note', _note),
  ('digest', _digest),
  ('newsletter', (run) => _newsletter(run, 112, 2)),
  ('large', (run) => _newsletter(run, 520, 6)),
];

class HtmlOpenApp extends StatefulWidget {
  const HtmlOpenApp({super.key});
  @override
  State<HtmlOpenApp> createState() => _HtmlOpenAppState();
}

class _HtmlOpenAppState extends State<HtmlOpenApp> {
  final _renderer = ValueNotifier(ReaderRenderer.fromName(Platform.environment['MAILSTREAM_READER_RENDERER'] ?? 'webKit'));
  String _status = 'Synthetic only · no accounts or private mail';

  /// The bodies mounted now: name, HTML and the opening's number.
  List<(String, String, int)> _shown = [];
  final _waiting = <int, Completer<void>>{};
  var _opening = 0;

  @override
  void initState() {
    super.initState();
    final out = Platform.environment['MAILSTREAM_HTML_OPEN_OUT'];
    if (out != null) Timer(const Duration(seconds: 2), () => _measure(out));
  }

  @override
  void dispose() {
    _renderer.dispose();
    super.dispose();
  }

  /// Mounts [mails] together; the milliseconds until each has its height.
  Future<List<double>> _open(List<(String, String Function(int))> mails) async {
    final clock = Stopwatch()..start();
    final shown = [for (final (name, html) in mails) (name, html(++_opening), _opening)];
    final times = <Future<double>>[];
    for (final (_, _, id) in shown) {
      final done = _waiting[id] = Completer<void>();
      times.add(done.future.timeout(const Duration(seconds: 20)).then((_) => clock.elapsedMicroseconds / 1000));
    }
    setState(() => _shown = shown);
    final result = await Future.wait(times);
    setState(() => _shown = []);
    await Future<void>.delayed(const Duration(milliseconds: 300));
    return result;
  }

  Future<void> _measure(String out) async {
    final root = Directory('$_inlineRoot/$_key');
    await root.create(recursive: true);
    await File('${root.path}/0-logo.png').writeAsBytes(base64Decode(_png));
    try {
      final single = <String, List<double>>{for (final (name, _) in _mails) name: []};
      final thread = <double>[];
      const warmup = 2, rounds = 14;
      for (var round = 0; round < warmup + rounds; round++) {
        // A different first mail each round, so none always opens on a cold page process.
        for (var i = 0; i < _mails.length; i++) {
          final mail = _mails[(round + i) % _mails.length];
          final [ms] = await _open([mail]);
          if (round >= warmup) single[mail.$1]!.add(ms);
          if (mounted) setState(() => _status = 'round ${round + 1} · ${mail.$1} · ${ms.toStringAsFixed(1)} ms');
        }
        final all = await _open(_mails);
        if (round >= warmup) thread.add(all.reduce((a, b) => a > b ? a : b));
      }
      double median(List<double> values) => (List.of(values)..sort())[values.length ~/ 2];
      double p90(List<double> values) => (List.of(values)..sort())[(values.length * 9 / 10).floor().clamp(0, values.length - 1)];
      final summary = {
        for (final (name, html) in _mails)
          name: {'kb': (html(0).length / 1024).round(), 'openMedianMs': median(single[name]!), 'openP90Ms': p90(single[name]!)},
        'thread': {'allFourMedianMs': median(thread), 'allFourP90Ms': p90(thread)},
      };
      await File(out).writeAsString(const JsonEncoder.withIndent(' ').convert({'renderer': _renderer.value.name, 'summary': summary, 'single': single, 'thread': thread}));
      stdout.writeln(jsonEncode(summary));
    } finally {
      await root.delete(recursive: true);
    }
    exit(0);
  }

  @override
  Widget build(BuildContext context) => MacosApp(
    theme: MailTheme.macos(Brightness.light),
    home: ReaderRendererScope(
      controller: _renderer,
      child: Container(
        color: const Color(0xfff0f2f5),
        padding: const EdgeInsets.fromLTRB(28, 48, 28, 28),
        child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
          Text('MS-234 · ${_renderer.value.title} body open time', style: TextStyle(fontSize: 20, color: Color(0xff18324f))),
          const SizedBox(height: 8),
          Text(_status, style: const TextStyle(fontSize: 13, color: Color(0xff51627a))),
          const SizedBox(height: 16),
          Expanded(child: SingleChildScrollView(child: Container(
            color: const Color(0xffffffff),
            padding: const EdgeInsets.all(20),
            child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
              for (final (name, html, id) in _shown)
                _Probe(
                  key: ValueKey(id),
                  onShown: () => _waiting.remove(id)?.complete(),
                  child: MailHtml(
                    owner: (account: 'synthetic', conversation: name, message: '$id'),
                    html: html,
                    onOpenLink: (_) {},
                  ),
                ),
            ]),
          ))),
        ]),
      ),
    ),
  );
}

/// Calls [onShown] once the first body in [child] (a WebKit view or a Blitz body) is taller than
/// its first 40 points: the page has laid out and Flutter has its height. (The fold's toggle does
/// not count.)
class _Probe extends StatefulWidget {
  const _Probe({super.key, required this.onShown, required this.child});
  final VoidCallback onShown;
  final Widget child;
  @override
  State<_Probe> createState() => _ProbeState();
}

class _ProbeState extends State<_Probe> {
  bool _check(SizeChangedLayoutNotification _) {
    Element? view;
    void visit(Element element) {
      if (view != null) return;
      if (element.widget is AppKitView || element.widget is BlitzBody) {
        view = element;
      } else {
        element.visitChildren(visit);
      }
    }

    context.visitChildElements(visit);
    final box = view?.renderObject;
    if (box is RenderBox && box.hasSize && box.size.height > 41) widget.onShown();
    return true;
  }

  @override
  Widget build(BuildContext context) => NotificationListener<SizeChangedLayoutNotification>(
    onNotification: _check,
    child: SizeChangedLayoutNotifier(child: widget.child),
  );
}
