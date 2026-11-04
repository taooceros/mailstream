import Foundation

extension Dictionary {
    /// Removes the entries whose key matches, handing each value to `cancel` before it goes, so
    /// work in flight for a removed key stops.
    nonisolated mutating func removeAll(whereKey isRemoved: (Key) -> Bool, cancelling cancel: (Value) -> Void = { _ in }) {
        for (key, value) in self where isRemoved(key) {
            cancel(value)
            self[key] = nil
        }
    }
}

extension MutableCollection where Self: RandomAccessCollection {
    /// Sorts newest first by `date`; undated elements sort as the oldest.
    nonisolated mutating func sortNewestFirst(by date: (Element) -> Date?) {
        sort { (date($0) ?? .distantPast) > (date($1) ?? .distantPast) }
    }
}
