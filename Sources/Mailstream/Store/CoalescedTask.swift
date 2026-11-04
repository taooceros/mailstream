/// Runs an async pass one at a time: requests made while a pass runs coalesce into one more pass
/// after it. The pass closure should capture its owner weakly: once the owner is gone, passes do
/// nothing and the task ends after the one in flight.
final class CoalescedTask {
    private let name: String
    private let pass: () async -> Void
    private var task: Task<Void, Never>?
    /// Another pass was requested since the running one started.
    private(set) var isPending = false

    init(_ name: String, pass: @escaping () async -> Void) {
        self.name = name
        self.pass = pass
    }

    var isRunning: Bool { task != nil }

    /// Runs a pass soon, or once more after the one in flight. A pass can check `isPending` to drop
    /// results a newer request already made stale.
    func request() {
        isPending = true
        guard task == nil else { return }
        task = Task(name: name) { await run() }
    }

    /// Requests a pass and returns once none is running or pending.
    func settle() async {
        request()
        while let task { await task.value }
    }

    /// Cancels the pass in flight and runs a fresh one after it.
    func restart() {
        task?.cancel()
        request()
    }

    /// Cancels the pass in flight and any requested after it.
    func cancel() {
        isPending = false
        task?.cancel()
    }

    private func run() async {
        while isPending, !Task.isCancelled {
            isPending = false
            await pass()
        }
        task = nil
        // `restart` cancelled the pass that just ended; the fresh one needs a task of its own.
        if isPending { request() }
    }
}
