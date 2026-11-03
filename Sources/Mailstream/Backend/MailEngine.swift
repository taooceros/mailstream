import CMailstreamEngine
import Foundation

/// The himalaya engine linked into the app (`Engine/`). A request is a himalaya argument list; it
/// runs on a thread of the engine's own and resumes the caller when it is done, so no Swift
/// thread ever waits on mail IO. Cancelling the caller stops waiting; the engine finishes the
/// command and keeps its connection.
enum MailEngine {
    /// Call once, before the first request. Token commands in the config (ortie, `security`) run
    /// from the engine with this process's environment, so they get the recovered login PATH.
    static func start(environment: [String: String]) {
        for key in ["PATH", "NO_COLOR"] {
            if let value = environment[key] { setenv(key, value, 1) }
        }
        unsetenv("RUST_LOG")
        ms_engine_init()
    }

    /// What the himalaya CLI would have printed on stdout, and its exit status.
    static func run(_ arguments: [String]) async throws -> (status: Int32, data: Data) {
        let request = try JSONEncoder().encode(arguments)
        let call = EngineCall()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                guard call.install(continuation) else { return }
                let context = Unmanaged.passRetained(call).toOpaque()
                request.withUnsafeBytes { bytes in
                    ms_engine_run(bytes.bindMemory(to: UInt8.self).baseAddress!, bytes.count, context) { context, status, output, length in
                        let call = Unmanaged<EngineCall>.fromOpaque(context!).takeRetainedValue()
                        call.finish(.success((status, output.map { Data(bytes: $0, count: length) } ?? Data())))
                    }
                }
            }
        } onCancel: {
            call.finish(.failure(CancellationError()))
        }
    }
}

/// One request's continuation, resumed exactly once: by the engine's answer or by cancellation,
/// whichever comes first.
private final class EngineCall: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<(status: Int32, data: Data), Error>?
    private var cancelled = false

    /// False when the caller was cancelled before the request started; it is then already resumed.
    func install(_ continuation: CheckedContinuation<(status: Int32, data: Data), Error>) -> Bool {
        let cancelled = lock.withLock {
            if !self.cancelled { self.continuation = continuation }
            return self.cancelled
        }
        if cancelled { continuation.resume(throwing: CancellationError()) }
        return !cancelled
    }

    func finish(_ result: Result<(status: Int32, data: Data), Error>) {
        let continuation = lock.withLock {
            defer { self.continuation = nil; cancelled = true }
            return self.continuation
        }
        continuation?.resume(with: result)
    }
}
