import AppKit

/// Reads a two-finger trackpad swipe out of scroll events: fingers moving sideways, either way,
/// drag the reader back. Gestures that start vertical pass through untouched, so the page scrolls.
struct BackSwipe {
    enum Step {
        /// Not a back swipe: deliver the event as usual.
        case pass
        /// Part of a back swipe with nothing to show (the momentum after release).
        case consume
        /// The reader follows the fingers, `offset` points to the right (negative: left).
        case drag(CGFloat)
        /// Fingers lifted; `velocity` in points per second, positive to the right.
        case release(offset: CGFloat, velocity: CGFloat)
    }

    private enum State { case idle, undecided, ignoring, tracking, coasting }
    private var state = State.idle
    private var offset: CGFloat = 0
    /// The side the swipe set off to (1 right, -1 left); the reader never crosses back past 0.
    private var direction: CGFloat = 1
    private var travel = CGVector.zero
    private var velocity: CGFloat = 0
    private var lastTime: TimeInterval = 0

    /// Movement it takes to tell a sideways swipe from a scroll.
    private static let decisionDistance: CGFloat = 6

    /// `canStart`: whether a gesture beginning with this event may become a back swipe.
    mutating func feed(_ event: NSEvent, canStart: Bool) -> Step {
        guard event.hasPreciseScrollingDeltas else { return .pass }
        if !event.momentumPhase.isEmpty {
            // The flick after a back swipe would otherwise scroll whatever ends up under the pointer.
            guard state == .coasting else { return .pass }
            if event.momentumPhase.contains(.ended) || event.momentumPhase.contains(.cancelled) { state = .idle }
            return .consume
        }
        if event.phase.contains(.began) {
            state = canStart ? .undecided : .ignoring
            offset = 0
            travel = .zero
            velocity = 0
            lastTime = event.timestamp
        }
        // Fingers to the right, whichever way the user's scrolling direction is set.
        let dx = event.isDirectionInvertedFromDevice ? event.scrollingDeltaX : -event.scrollingDeltaX
        let dy = event.scrollingDeltaY
        let lifted = event.phase.contains(.ended) || event.phase.contains(.cancelled)

        switch state {
        case .idle, .coasting:
            return .pass
        case .ignoring:
            if lifted { state = .idle }
            return .pass
        case .undecided:
            if lifted {
                state = .idle
                return .pass
            }
            travel.dx += dx
            travel.dy += dy
            guard hypot(travel.dx, travel.dy) >= Self.decisionDistance else { return .pass }
            guard abs(travel.dx) > abs(travel.dy) * 1.5 else {
                state = .ignoring
                return .pass
            }
            state = .tracking
            direction = travel.dx > 0 ? 1 : -1
            offset = travel.dx
            lastTime = event.timestamp
            return .drag(offset)
        case .tracking:
            if lifted {
                state = .coasting
                let released = event.phase.contains(.cancelled) ? 0 : offset
                return .release(offset: released, velocity: velocity)
            }
            let dt = event.timestamp - lastTime
            lastTime = event.timestamp
            if dt > 0 { velocity = velocity * 0.5 + dx / CGFloat(dt) * 0.5 }
            offset = direction * max(0, direction * (offset + dx))
            return .drag(offset)
        }
    }
}
