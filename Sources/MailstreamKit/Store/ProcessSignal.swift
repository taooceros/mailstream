import Foundation
import notify

/// A change every Mailstream process on this Mac should pick up: the app, front ends running the
/// engine in-process (GPUI, Flutter), and the app the CLI talks to share mail.db but hold what
/// they show in memory. A Darwin notification carries no data, so receivers read the change from
/// mail.db; the posting process hears its own posts too.
nonisolated struct ProcessSignal: Sendable {
    /// AI tags written (`MailCache.saveTags`, `clearTags`).
    static let tags = ProcessSignal(name: "dev.mailstream.Mailstream.tags")

    let name: String

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
}
