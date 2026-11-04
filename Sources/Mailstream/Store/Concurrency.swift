/// Runs `work` for every job at once and hands each outcome to `consume` on the caller's actor as
/// it lands, in completion order, so results apply progressively rather than after the slowest.
func forEachConcurrently<Job: Sendable, Output: Sendable>(
    _ jobs: [Job], _ work: @escaping @Sendable (Job) async throws -> Output,
    consume: (Job, Result<Output, any Error>) -> Void
) async {
    await withTaskGroup(of: (Job, Result<Output, any Error>).self) { group in
        for job in jobs {
            group.addTask {
                // Bound before the tuple is built: Swift 6.3 at -O drops the first element
                // from `(job, .success(try await …))` on success.
                do {
                    let output = try await work(job)
                    return (job, .success(output))
                } catch {
                    return (job, .failure(error))
                }
            }
        }
        for await (job, result) in group { consume(job, result) }
    }
}

/// Runs `work` for every job at once and returns when all have finished.
func forEachConcurrently<Job: Sendable>(_ jobs: [Job], _ work: @escaping @Sendable (Job) async -> Void) async {
    await withTaskGroup(of: Void.self) { group in
        for job in jobs { group.addTask { await work(job) } }
    }
}

/// A loop calling `tick` on its owner every `interval`, until cancelled or the owner is gone.
struct PeriodicTask {
    private var running: (interval: Duration, task: Task<Void, Never>)?

    /// Starts the loop; a changed interval starts the wait afresh, the one already running keeps it.
    mutating func schedule<Owner: AnyObject>(every interval: Duration, name: String, on owner: Owner,
                                             _ tick: @escaping (Owner) -> Void) {
        if running?.interval == interval { return }
        running?.task.cancel()
        running = (interval, Task(name: name) { [weak owner] in
            while true {
                do { try await Task.sleep(for: interval) } catch { return }
                guard let owner else { return }
                tick(owner)
            }
        })
    }

    func cancel() { running?.task.cancel() }
}
