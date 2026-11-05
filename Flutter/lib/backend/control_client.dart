import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

/// A failed command: the app's stable `code` and a message for people.
class ControlException implements Exception {
  ControlException(this.code, this.message);

  final String code;
  final String message;

  @override
  String toString() => message;
}

/// The running Mailstream app's control socket
/// (`~/Library/Application Support/Mailstream/control.sock`): one JSON request per connection,
/// answered with one JSON object before the app closes the connection. The Swift app stays the
/// backend (accounts, sync, cache, threading, tagging); this UI only asks it for values.
class ControlClient {
  ControlClient({String? path}) : path = path ?? '${Platform.environment['HOME']}/Library/Application Support/Mailstream/control.sock';

  final String path;

  /// Answers above this size decode on another isolate, so a long thread never stalls a frame.
  static const _inlineDecodeLimit = 48 * 1024;

  Future<Object?> call(String command, [Map<String, Object?> args = const {}]) async {
    final Socket socket;
    try {
      socket = await Socket.connect(InternetAddress(path, type: InternetAddressType.unix), 0, timeout: const Duration(seconds: 3));
    } on SocketException {
      throw ControlException('unreachable', 'Mailstream is not running: nothing listens on its control socket.');
    }
    try {
      socket.add(utf8.encode('${jsonEncode({'command': command, 'args': args})}\n'));
      await socket.flush();
      final builder = BytesBuilder(copy: false);
      await for (final chunk in socket) {
        builder.add(chunk);
      }
      final bytes = builder.takeBytes();
      final answer = bytes.length > _inlineDecodeLimit ? await Isolate.run(() => _decode(bytes)) : _decode(bytes);
      if (answer['ok'] == true) return answer['result'];
      final error = answer['error'] as Map<String, Object?>? ?? const {};
      throw ControlException(error['code'] as String? ?? 'failed', error['message'] as String? ?? 'The command failed.');
    } on SocketException catch (error) {
      throw ControlException('unreachable', 'The connection to Mailstream broke: ${error.message}');
    } finally {
      socket.destroy();
    }
  }
}

Map<String, Object?> _decode(Uint8List bytes) {
  if (bytes.isEmpty) throw ControlException('unreachable', 'Mailstream closed the connection without answering.');
  return jsonDecode(utf8.decode(bytes)) as Map<String, Object?>;
}
