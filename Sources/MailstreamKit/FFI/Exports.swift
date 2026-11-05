import Foundation

/// The engine's C API, for front ends in other languages that load `libMailstreamKit.dylib`.
/// The Rust crate in `FFI/` is its one client; UniFFI generates the Dart (and other) bindings
/// from that crate, so nothing else should call these directly.
///
/// Requests and answers are the control socket's JSON (`ControlService`): a request
/// `{"command": "mail.list", "args": {…}}` answers `{"ok": true, "result": …}` or
/// `{"ok": false, "error": {"code", "message"}}`. Here they cross in-process instead of over the
/// socket. Events are `{"event": "<what changed>"}`; a front end re-reads what it shows.
///
/// The engine is main-actor code: the host must run the main thread's run loop (an AppKit,
/// Flutter or GPUI app does; a command-line host calls `mailstream_run_main_loop`). Callbacks run
/// on the main thread; the bytes they get are valid only during the call.
public typealias MailstreamCallback = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void

/// A C callback with its context, handed to the main actor.
private nonisolated struct Callback: @unchecked Sendable {
    let context: UnsafeMutableRawPointer?
    let function: MailstreamCallback

    func callAsFunction(_ data: Data) {
        data.withUnsafeBytes { bytes in
            function(context, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count)
        }
    }
}

/// Starts the engine (accounts, sync, cache, push, tagging) if it is not running yet.
@_cdecl("mailstream_start")
public nonisolated func mailstreamStart() {
    Task { @MainActor in _ = HostedEngine.shared }
}

/// Runs one request; `reply` gets the answer once, on the main thread.
@_cdecl("mailstream_call")
public nonisolated func mailstreamCall(_ request: UnsafePointer<UInt8>?, _ length: Int,
                                       _ context: UnsafeMutableRawPointer?, _ reply: MailstreamCallback) {
    let data = request.map { Data(bytes: $0, count: length) } ?? Data()
    let callback = Callback(context: context, function: reply)
    Task { @MainActor in
        let answer = await HostedEngine.shared.service.handle(data)
        callback(answer)
    }
}

/// Calls `event` with each change from now on, on the main thread, for as long as the process runs.
@_cdecl("mailstream_subscribe")
public nonisolated func mailstreamSubscribe(_ context: UnsafeMutableRawPointer?, _ event: MailstreamCallback) {
    let callback = Callback(context: context, function: event)
    Task { @MainActor in HostedEngine.shared.listeners.append(callback) }
}

/// Runs the main thread's run loop for a host without one; never returns. Call on the main thread.
@_cdecl("mailstream_run_main_loop")
public nonisolated func mailstreamRunMainLoop() {
    while true { CFRunLoopRun() }
}

/// The engine as an FFI host runs it: the app's stores without its windows or control socket
/// (the app, when it runs too, keeps serving `mailstream` commands).
private final class HostedEngine {
    static let shared = HostedEngine()

    let service: ControlService
    fileprivate var listeners: [Callback] = []
    private let store: MailStore
    private var watches: [Task<Void, Never>] = []

    private init() {
        Settings.shared.startWatching()
        let accounts = AccountStore(problems: StoreProblems())
        store = MailStore(accounts: accounts, tools: ToolEnvironment.resolve())
        service = ControlService(accounts: accounts, store: store)
        store.start()
        watch()
    }

    /// One event per kind of change the front ends show.
    private func watch() {
        let store = store
        watches = [
            follow("accounts", Observations { _ = store.accounts }),
            follow("mailboxes", Observations { _ = store.mailboxes }),
            follow("conversations", Observations { _ = store.conversations }),
            follow("previews", Observations { _ = store.contents.snippets }),
            follow("tags", Observations { _ = store.tagging.tags }),
            follow("sync", Observations { _ = (store.isLoading, store.isPushConnected) }),
            follow("problems", Observations { _ = store.problems.current }),
            follow("settings", Observations { _ = (Settings.shared.values, Settings.shared.issues) }),
            follow("chat", Observations { _ = (store.chat.accounts, store.chat.rows, store.chat.isLoading) }),
            // The open space's history: fetched pages, earlier pages, sends confirmed or failed.
            follow("chatMessages", Observations {
                if let key = store.chat.openSpace { _ = (store.chat.thread(key), store.chat.isLoadingEarlier(key)) }
            }),
        ]
    }

    private func follow(_ event: String, _ changes: Observations<Void, Never>) -> Task<Void, Never> {
        let message = JSON.object(["event": .string(event)]).encoded(pretty: false)
        return Task.immediate(name: "FFI \(event) events") { [weak self] in
            for await _ in changes.dropFirst() {
                self?.listeners.forEach { $0(message) }
            }
        }
    }
}
