import Foundation
import Observation

/// A user-facing failure reported by a store.
nonisolated struct StoreProblem: Identifiable, Equatable, Sendable {
    /// Which status line shows it; account-list failures only raise an alert.
    enum Source: Sendable { case mail, chat, accounts }

    let id = UUID()
    let source: Source
    let message: String
    /// Interrupts with an alert: a user action failed.
    let alert: Bool
}

/// The one channel every store reports failures into. Status lines observe their source's current
/// problem, the main window drains `alerts`.
@Observable
final class StoreProblems {
    /// The latest problem of the mail and chat sources, until that source clears it (a refresh
    /// starts, a poll succeeds).
    private(set) var current: [StoreProblem.Source: StoreProblem] = [:]
    /// Problems that asked for an alert and were not shown yet, oldest first.
    private(set) var alerts: [StoreProblem] = []

    /// Records `error` (prefixed with `context`, usually the account); cancellations are not problems.
    func report(_ error: any Error, from source: StoreProblem.Source, context: String? = nil, alert: Bool = false) {
        if error is CancellationError { return }
        report(context.map { "\($0): \(error.localizedDescription)" } ?? error.localizedDescription, from: source,
               alert: alert)
    }

    func report(_ message: String, from source: StoreProblem.Source, alert: Bool = false) {
        let problem = StoreProblem(source: source, message: message, alert: alert)
        if source != .accounts { current[source] = problem }
        if alert { alerts.append(problem) }
    }

    /// The pending alerts, which leave the queue.
    func takeAlerts() -> [StoreProblem] {
        guard !alerts.isEmpty else { return [] }
        defer { alerts = [] }
        return alerts
    }

    func clear(_ source: StoreProblem.Source) {
        if current[source] != nil { current[source] = nil }
    }
}
