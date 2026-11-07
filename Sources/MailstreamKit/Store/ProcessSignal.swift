import Foundation
#if canImport(notify)
import notify
#else
import Synchronization
#endif

/// A change every Mailstream process on this machine should pick up: the app, front ends running
/// the engine in-process (GPUI, Flutter), and the app the CLI talks to share mail.db but hold what
/// they show in memory. On the Mac a Darwin notification carries it; it carries no data, so
/// receivers read the change from mail.db. Windows has one such process (the Flutter app), so the
/// signal stays in it. Either way the posting process hears its own posts too.
nonisolated struct ProcessSignal: Sendable {
    /// AI tags written (`MailCache.saveTags`, `clearTags`).
    static let tags = ProcessSignal(name: "dev.mailstream.Mailstream.tags")

    let name: String

    #if canImport(notify)
    func post() {
        notify_post(name)
    }

    /// One element per post from now on, posts close together coalesced; stops listening when the
    /// iteration ends.
    func posts() -> AsyncStream<Void> {
        let (stream, continuation) = AsyncStream.makeStream(of: Void.self, bufferingPolicy: .bufferingNewest(1))
        var token: Int32 = 0
        let status = notify_register_dispatch(name, &token, .global(qos: .utility)) { _ in continuation.yield() }
        guard status == NOTIFY_STATUS_OK else {
            continuation.finish()
            return stream
        }
        let registration = token
        continuation.onTermination = { _ in notify_cancel(registration) }
        return stream
    }
    #else
    /// Listeners by signal name, then by registration.
    private static let listeners = Mutex<[String: [UUID: AsyncStream<Void>.Continuation]]>([:])

    func post() {
        let current = Self.listeners.withLock { Array(($0[name] ?? [:]).values) }
        current.forEach { $0.yield() }
    }

    /// One element per post from now on, posts close together coalesced; stops listening when the
    /// iteration ends.
    func posts() -> AsyncStream<Void> {
        let (stream, continuation) = AsyncStream.makeStream(of: Void.self, bufferingPolicy: .bufferingNewest(1))
        let id = UUID()
        let name = name
        Self.listeners.withLock { $0[name, default: [:]][id] = continuation }
        continuation.onTermination = { _ in Self.listeners.withLock { _ = $0[name]?.removeValue(forKey: id) } }
        return stream
    }
    #endif
}
