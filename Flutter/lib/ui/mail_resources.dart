import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:html/dom.dart' as dom;
import 'package:html/parser.dart' as html_parser;

const maxInlineImageBytes = 20 * 1024 * 1024;
const rasterMimeTypes = {'image/png', 'image/jpeg', 'image/gif', 'image/webp'};

/// The engine's inline-part cache (`cache_directory()` in `Engine/src/lib.rs`, after the `dirs`
/// crate): `~/Library/Caches` on the Mac, `$XDG_CACHE_HOME` (else `~/.cache`) on Linux.
String get inlineCacheRoot {
  final environment = Platform.environment;
  final xdg = environment['XDG_CACHE_HOME'];
  final caches = !Platform.isLinux
      ? '${environment['HOME']}/Library/Caches'
      : xdg != null && xdg.startsWith('/')
          ? xdg
          : '${environment['HOME']}/.cache';
  return '$caches/dev.mailstream.Mailstream/messages-v2';
}

Uint8List? inlineImageBytes(String source) {
  try {
    final data = Uri.parse(source).data;
    if (data == null || !rasterMimeTypes.contains(data.mimeType.toLowerCase()) || source.length > maxInlineImageBytes * 2) return null;
    final bytes = data.contentAsBytes();
    return bytes.length <= maxInlineImageBytes ? bytes : null;
  } on FormatException {
    return null;
  }
}

/// Read only canonical descendants of the application's existing inline-part cache.
/// File URLs never reach Blitz (its page reads no file): valid raster bytes become sanitized data URLs.
Future<String?> cachedImageData(String source, {String? cacheRoot}) async {
  try {
    final uri = Uri.parse(source);
    if (uri.scheme != 'file' || uri.host.isNotEmpty || uri.hasQuery || uri.hasFragment) return null;
    final root = await Directory(cacheRoot ?? inlineCacheRoot).resolveSymbolicLinks();
    final file = File.fromUri(uri);
    final canonical = await file.resolveSymbolicLinks();
    if (!canonical.startsWith('$root/')) return null;
    final resolved = File(canonical);
    if (await resolved.length() > maxInlineImageBytes) return null;
    final bytes = await resolved.readAsBytes();
    // MIME comes from the bytes, not an attacker-controlled extension.
    final mime = rasterMime(bytes);
    if (mime == null) return null;
    return 'data:$mime;base64,${base64Encode(bytes)}';
  } on FileSystemException {
    return null;
  } on FormatException {
    return null;
  } on ArgumentError {
    return null;
  }
}

String? rasterMime(List<int> b) {
  if (b.length >= 8 && b[0] == 137 && b[1] == 80 && b[2] == 78 && b[3] == 71 && b[4] == 13 && b[5] == 10 && b[6] == 26 && b[7] == 10) return 'image/png';
  if (b.length >= 3 && b[0] == 255 && b[1] == 216 && b[2] == 255) return 'image/jpeg';
  if (b.length >= 6 && ascii.decode(b.take(6).toList(), allowInvalid: true).startsWith('GIF8')) return 'image/gif';
  if (b.length >= 12 && ascii.decode(b.take(4).toList(), allowInvalid: true) == 'RIFF' && ascii.decode(b.sublist(8, 12), allowInvalid: true) == 'WEBP') return 'image/webp';
  return null;
}

bool isMailLink(String source) {
  final uri = Uri.tryParse(source);
  return uri != null && {'https', 'http', 'mailto'}.contains(uri.scheme.toLowerCase()) && uri.userInfo.isEmpty && (uri.scheme == 'mailto' || uri.host.isNotEmpty);
}

/// The policy WebKit applies to a body: HTTPS and data images, styles and fonts, and the inline
/// images the runner serves at `mscid:` (`InlinePartHandler` in `MailHtmlView.swift`), nothing
/// else. No `file:` source; mail scripts stay disabled in the view's configuration.
const webKitContentPolicy = "default-src 'none'; img-src https: data: mscid:; style-src 'unsafe-inline' https:; "
    "font-src https: data:; base-uri 'none'; form-action 'none'";

/// What may come before a doctype without putting the page in quirks mode: a byte-order mark,
/// white space, comments and the `<?xml …?>` line XHTML mail starts with (a bogus comment to HTML).
final _webKitPrologue = RegExp(r'^(?:\s|\uFEFF|<!--[\s\S]*?-->|<\?[^>]*>)*<!doctype[^>]*>', caseSensitive: false);

/// The engine's `file:` URL of the inline cache, as it writes the folder of each message's inline
/// parts (`MailCache::message_directory_url`: the path percent-encoded as `URL` paths are).
final inlineCacheUrl = Uri.directory(inlineCacheRoot).toString();

/// The page a WebKit body loads: the mail exactly as the engine gave it, with two string edits
/// and no parsing (MS-167). Its inline cache images are addressed at `mscid://<message>/<part>`,
/// which the runner serves, instead of the `file:` URLs the engine writes (a page may read no
/// file); [webKitContentPolicy] goes ahead of everything but a leading doctype, so the parser puts
/// it into the (implied) head and a standards-mode body is not laid out in quirks mode.
String webKitDocument(String html) {
  const meta = '<meta http-equiv="Content-Security-Policy" content="$webKitContentPolicy">';
  if (html.contains('file:')) html = html.replaceAll(inlineCacheUrl, 'mscid://');
  final at = _webKitPrologue.firstMatch(html)?.end ?? 0;
  return '${html.substring(0, at)}$meta${html.substring(at)}';
}

/// [document] as HTML, its doctype keeping the PUBLIC or SYSTEM keyword before its identifiers.
/// package:html writes them without it (`<!DOCTYPE html "-//W3C//DTD XHTML 1.0 Transitional//EN"
/// "…">`), which browsers read as a broken doctype: the mail then lays out in quirks mode instead
/// of the mode it was written for (MS-160).
String documentHtml(dom.Document document) {
  final doctype = document.nodes.whereType<dom.DocumentType>().firstOrNull;
  if (doctype != null && doctype is! _Doctype) doctype.replaceWith(_Doctype(doctype));
  return document.outerHtml;
}

class _Doctype extends dom.DocumentType {
  _Doctype(dom.DocumentType doctype) : super(doctype.name, doctype.publicId, doctype.systemId);

  @override
  String toString() {
    String quoted(String id) => id.contains('"') ? "'$id'" : '"$id"';
    final name = this.name ?? '';
    return switch ((publicId, systemId)) {
      (null, null) => '<!DOCTYPE $name>',
      (final public?, null) => '<!DOCTYPE $name PUBLIC ${quoted(public)}>',
      (null, final system?) => '<!DOCTYPE $name SYSTEM ${quoted(system)}>',
      (final public?, final system?) => '<!DOCTYPE $name PUBLIC ${quoted(public)} ${quoted(system)}>',
    };
  }
}

/// The image policy of the bodies Blitz draws (WebKit loads the mail as it came, [webKitDocument]):
/// inline cache images become data URLs ([cachedImageData]), raster `data:` and HTTPS images stay,
/// and every other source goes, its `alt` saying why. `srcset` goes too.
Future<String> prepareMailImages(String html) async {
  if (!html.toLowerCase().contains('<img')) return html;
  final document = html_parser.parse(html);
  for (final image in document.querySelectorAll('img')) {
    final source = image.attributes['src'] ?? '';
    final uri = Uri.tryParse(source);
    String? safe;
    if (uri?.scheme == 'file') {
      safe = await cachedImageData(source);
    } else if (uri?.scheme == 'data' && inlineImageBytes(source) != null) {
      safe = source;
    } else if (uri?.scheme == 'https' && uri!.host.isNotEmpty && uri.userInfo.isEmpty) {
      safe = source;
    }
    image.attributes.remove('srcset');
    if (safe == null) {
      image.attributes.remove('src');
      image.attributes['alt'] = '[Image unavailable: unsupported or outside inline cache] ${image.attributes['alt'] ?? ''}';
    } else {
      image.attributes['src'] = safe;
    }
  }
  return documentHtml(document);
}
