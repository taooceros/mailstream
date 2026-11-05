import Foundation

/// A provider's per-user quota as a token bucket: each request spends its cost, waiting while
/// the bucket is low. Background requests leave `reserve` units for interactive ones, so a
/// prefetch burst never delays opening a message. A refusal for load pauses and empties it.
actor QuotaBudget {
    private let capacity: Double
    private let perSecond: Double
    private let reserve: Double
    private var available: Double
    private var updated = ContinuousClock.now
    private var pausedUntil = ContinuousClock.now

    /// In any minute at most `burst + perMinute` units go out.
    init(perMinute: Int, burst: Int, reserve: Int) {
        capacity = Double(burst)
        perSecond = Double(perMinute) / 60
        self.reserve = Double(reserve)
        available = Double(burst)
    }

    /// Gmail's per-user quota refused this app's mail well under the documented 15,000 units a
    /// minute (messages.get costs 5): about 1,400 units in half a minute, mostly 250-unit
    /// batches. So the pace is a quarter of that and bursts stay small.
    static func gmail() -> QuotaBudget {
        QuotaBudget(perMinute: 3000, burst: 300, reserve: 75)
    }

    /// Nothing is spent for `delay`, and the bucket refills from empty afterwards.
    func pause(for delay: Duration) {
        let until = ContinuousClock.now + delay
        if until > pausedUntil { pausedUntil = until }
        available = 0
        updated = pausedUntil
    }

    func spend(_ cost: Int, priority: RequestPriority) async throws {
        let cost = min(Double(cost), capacity)
        let floor = priority == .background ? reserve : 0
        while true {
            let now = ContinuousClock.now
            if now < pausedUntil {
                try await Task.sleep(until: pausedUntil, clock: .continuous)
                continue
            }
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
