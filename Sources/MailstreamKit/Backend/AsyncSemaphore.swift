/// FIFO counting semaphore bounding concurrent work. A task cancelled while it waits for a
/// permit leaves the queue and throws `CancellationError`.
actor AsyncSemaphore {
    private var available: Int
    private var waiters: [(id: Int, continuation: CheckedContinuation<Void, any Error>)] = []
    private var nextWaiter = 0

    init(_ count: Int) { available = count }

    /// Runs `body` holding one permit, on the caller's actor.
    nonisolated func withPermit<T>(_ body: () async throws -> T) async throws -> T {
        try await acquire()
        do {
            let value = try await body()
            await release()
            return value
        } catch {
            await release()
            throw error
        }
    }

    private func acquire() async throws {
        try Task.checkCancellation()
        if available > 0 {
            available -= 1
            return
        }
        let id = nextWaiter
        nextWaiter += 1
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { waiters.append((id, $0)) }
        } onCancel: {
            // The hop runs once `acquire` has suspended, so the waiter is queued by then.
            Task { await self.leave(id) }
        }
    }

    private func release() {
        if waiters.isEmpty {
            available += 1
        } else {
            waiters.removeFirst().continuation.resume()
        }
    }

    /// Drops a cancelled waiter that has not been granted a permit yet.
    private func leave(_ id: Int) {
        guard let index = waiters.firstIndex(where: { $0.id == id }) else { return }
        waiters.remove(at: index).continuation.resume(throwing: CancellationError())
    }
}
