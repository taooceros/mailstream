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

    /// Maximum units one Gmail batch request may spend, so no batch exceeds the burst.
    static let gmailBatchUnits = 1000

    /// Gmail's per-user quota as Google publishes it since 2026-05-01: 6,000 units a minute,
    /// `messages.get` 20 units (60 with `format=full`), every request's cost from the table at
    /// <https://developers.google.com/workspace/gmail/api/reference/quota> (MS-356). In any
    /// minute at most `perMinute + burst` units go out: 4,500 + 1,200 stays under 6,000, and the
    /// 150-unit reserve leaves room for message opens while a 1,000-unit batch runs.
    static func gmail() -> QuotaBudget {
        QuotaBudget(perMinute: 4500, burst: 1200, reserve: 150)
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
            let target = floor > 0 && cost > capacity - floor ? capacity : cost + floor
            let missing = target - available
            try await Task.sleep(for: .milliseconds(Int(missing / perSecond * 1000) + 5))
        }
    }
}
