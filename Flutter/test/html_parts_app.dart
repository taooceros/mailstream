import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';
import 'package:macos_ui/macos_ui.dart';
import 'package:mailstream_ui/ui/design.dart';
import 'package:mailstream_ui/ui/html_parts.dart';
import 'package:mailstream_ui/ui/reader.dart';
import 'package:mailstream_ui/ui/controls.dart';

// Release-only synthetic runtime evidence; never starts the engine or reads private mail.
// flutter run -d macos --release -t test/html_parts_app.dart
// MAILSTREAM_HTML_BENCH_OUT=/tmp/html-parts.json enables the paired comparison.
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await WindowManipulator.initialize(enableWindowDelegate: true);
  runApp(const HtmlPartsApp());
}

String sample(int rows, int revision) => '<html><head><style>'
    'body{font:15px -apple-system;color:#18324f}table{border-collapse:collapse}'
    'td{padding:3px 12px;border-bottom:1px solid #ddd}.history{color:#51627a}'
    '</style></head><body><h2>Synthetic HTML · revision $revision</h2>'
    '<p>Current message stays visible. Quoted history starts behind the ellipsis.</p>'
    '<table>${List.generate(rows, (i) => '<tr><td>Item $i</td><td><b>Current</b> &amp; styled content <a href="https://example.test/$i">sample link</a></td></tr>').join()}</table>'
    '<div class="gmail_quote history"><h3>Gmail history</h3><p>Folded synthetic conversation.</p>'
    '<blockquote type="cite">Nested Apple Mail history appears only once.</blockquote></div>'
    '<div class="gmail_quote_container history">Gmail container history</div>'
    '<div class="moz-cite-prefix history">Thunderbird citation</div>'
    '<div id="divRplyFwdMsg" class="history">Outlook reply history</div>'
    '<div id="appendonsend" class="history">Outlook appended history</div>'
    '<blockquote type="cite" class="history">Apple Mail citation</blockquote>'
    '</body></html>';

class HtmlPartsApp extends StatefulWidget {
  const HtmlPartsApp({super.key});
  @override
  State<HtmlPartsApp> createState() => _HtmlPartsAppState();
}

class _HtmlPartsAppState extends State<HtmlPartsApp> {
  int _revision = 0;
  String _status = 'Synthetic only · no accounts or private mail';

  @override
  void initState() {
    super.initState();
    final out = Platform.environment['MAILSTREAM_HTML_BENCH_OUT'];
    if (out != null) Timer(const Duration(seconds: 2), () => _measure(out));
  }

  Future<void> _measure(String out) async {
    final results = <Map<String, Object>>[];
    for (final rows in [2, 2500]) {
      for (var run = 0; run < 20; run++) {
        // Alternate order to expose warm-up/load effects rather than picking a favorable order.
        for (final mode in run.isEven ? ['baseline', 'background'] : ['background', 'baseline']) {
          final html = sample(rows, run);
          final cache = HtmlPartsCache();
          final gaps = <int>[];
          final clock = Stopwatch()..start();
          var last = clock.elapsedMicroseconds;
          final heartbeat = Timer.periodic(const Duration(milliseconds: 1), (_) {
            final now = clock.elapsedMicroseconds;
            gaps.add(now - last);
            last = now;
          });
          final frameWork = <int>[];
          void timings(List<FrameTiming> frames) {
            frameWork.addAll(frames.map((frame) => frame.buildDuration.inMicroseconds));
          }
          SchedulerBinding.instance.addTimingsCallback(timings);
          await Future<void>.delayed(const Duration(milliseconds: 20));
          final watch = Stopwatch()..start();
          final HtmlParts parts;
          int dispatch;
          if (mode == 'baseline') {
            parts = splitMailHtml(html); // Exact pre-MS-87 algorithm from a1fc4b3d.
            dispatch = watch.elapsedMicroseconds;
          } else {
            final pending = cache.load(html);
            dispatch = watch.elapsedMicroseconds;
            parts = await pending;
          }
          final latency = watch.elapsedMicroseconds;
          // Real output equivalence, not merely a non-empty result assertion.
          final expected = splitMailHtml;
          // Compare outside the measured heartbeat/window below.
          await Future<void>.delayed(const Duration(milliseconds: 20));
          heartbeat.cancel();
          SchedulerBinding.instance.removeTimingsCallback(timings);
          if (parts != expected(html)) throw StateError('baseline/current output mismatch');
          if (parts.$1.contains('Gmail history') || !parts.$2!.contains('Apple Mail citation')) {
            throw StateError('quote isolation changed');
          }
          final hitWatch = Stopwatch()..start();
          if (mode == 'background') {
            if (await cache.load(html) != parts) throw StateError('cache output mismatch');
          }
          results.add({
            'mode': mode, 'rows': rows, 'run': run, 'inputCodeUnits': html.length,
            'dispatchUs': dispatch, 'latencyUs': latency,
            'maxHeartbeatGapUs': gaps.reduce((a, b) => a > b ? a : b),
            'frameBuildUs': frameWork,
            if (mode == 'background') 'cacheHitUs': hitWatch.elapsedMicroseconds,
          });
          if (mounted) setState(() => _status = '$mode · $rows rows · run ${run + 1}/20');
          await Future<void>.delayed(const Duration(milliseconds: 40));
        }
      }
    }
    await File(out).writeAsString(jsonEncode({'baselineRevision': 'a1fc4b3d43e4', 'runtime': Platform.version, 'results': results}));
    if (mounted) setState(() => _status = 'Paired runtime measurement complete · synthetic data only');
  }

  @override
  Widget build(BuildContext context) => MacosApp(
    theme: MailTheme.macos(Brightness.light),
    home: Container(
      color: const Color(0xfff0f2f5),
      padding: const EdgeInsets.fromLTRB(28, 48, 28, 28),
      child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
        Text('MS-87 · Background HTML reader', style: const TextStyle(fontSize: 20, color: Color(0xff18324f))),
        const SizedBox(height: 8),
        Text(_status, style: const TextStyle(fontSize: 13, color: Color(0xff51627a))),
        const SizedBox(height: 12),
        Row(children: [
          MailButton(label: 'Refresh synthetic message', onPressed: () => setState(() => _revision++)),
          const SizedBox(width: 12),
          MailButton(label: 'Reopen cached message', onPressed: () => setState(() => _revision = 0)),
        ]),
        const SizedBox(height: 16),
        Expanded(child: SingleChildScrollView(child: Container(
          color: const Color(0xffffffff), padding: const EdgeInsets.all(20),
          child: MailHtml(
            owner: (account: 'synthetic', conversation: 'synthetic', message: 'synthetic'),
            html: sample(3, _revision),
            onOpenLink: (_) {},
          ),
        ))),
      ]),
    ),
  );
}
