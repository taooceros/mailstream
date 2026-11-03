import Foundation

/// A provider's per-user quota as a token bucket: each request spends its cost, waiting while
/// the bucket is low. Background requests leave `reserve` units for interactive ones, so a
/// prefetch burst never delays opening a message.
actor QuotaBudget {
    private let capacity: Double
    private let perSecond: Double
    private let reserve: Double
    private var available: Double
    private var updated = ContinuousClock.now

    /// In any minute at most `burst + perMinute` units go out.
    init(perMinute: Int, burst: Int, reserve: Int) {
        capacity = Double(burst)
        perSecond = Double(perMinute) / 60
        self.reserve = Double(reserve)
        available = Double(burst)
    }

    /// Gmail allows 6,000 units per user per minute (messages.get costs 5) and also refuses
    /// bursts well inside that, so the bucket stays small.
    static func gmail() -> QuotaBudget {
        QuotaBudget(perMinute: 4800, burst: 600, reserve: 150)
    }

    func spend(_ cost: Int, priority: RequestPriority) async throws {
        let cost = min(Double(cost), capacity)
        let floor = priority == .background ? reserve : 0
        while true {
            let now = ContinuousClock.now
            let elapsed = updated.duration(to: now)
            updated = now
            available = min(capacity, available + perSecond * (Double(elapsed.components.seconds)
                + Double(elapsed.components.attoseconds) / 1e18))
            if available - cost >= floor || (floor > 0 && cost > capacity - floor && available >= capacity) {
                available -= cost
                return
            }
            let missing = cost + floor - available
            try await Task.sleep(for: .milliseconds(Int(missing / perSecond * 1000) + 5))
        }
    }
}
