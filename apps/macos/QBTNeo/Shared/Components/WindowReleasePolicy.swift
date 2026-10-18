/// Decides when a window that nobody sees drops its view tree
/// (docs/hidden-ui-memory.md). Pure state with an injected clock, so the
/// timing rules are unit-tested; `WindowContentReleaser` runs the timer.
///
/// * Closed: release after `closedDelay` (2 min).
/// * Fully occluded: release after `occludedDelay` (10 min).
/// * Minimized or app hidden (⌘H): never. AppKit keeps the views SwiftUI
///   removes until such a window is displayed again, so a release frees
///   nothing while it is hidden and its rebuild only adds fragmentation.
/// * The time counts from the moment the window stopped being visible; any
///   visibility in between starts it over. Changing from one hidden state to
///   another keeps the start, and the delay of the current state applies.
/// * Memory pressure releases a closed window at once; an occluded one can
///   be uncovered at any moment and waits for its delay.
/// * When the window cannot be released right now (a sheet is open, a field
///   is being edited…), the next attempt is `retryDelay` later.
struct WindowReleasePolicy<Instant: InstantProtocol>: Sendable where Instant.Duration == Duration {
    struct Delays: Sendable, Equatable {
        var closed = Duration.seconds(120)
        var occluded = Duration.seconds(600)
        var retry = Duration.seconds(30)
    }

    let delays: Delays
    private(set) var visibility = WindowVisibility.visible
    /// When the window stopped being visible.
    private(set) var hiddenSince: Instant?
    private(set) var isReleased = false
    private var retryAt: Instant?

    init(delays: Delays = Delays()) {
        self.delays = delays
    }

    /// Records a visibility change.
    mutating func update(_ next: WindowVisibility, at now: Instant) {
        if next == .visible {
            hiddenSince = nil
            retryAt = nil
        } else if hiddenSince == nil {
            hiddenSince = now
        }
        visibility = next
    }

    /// When the window should drop its views, if it is hidden in a way that
    /// allows it and still has them.
    var releaseDeadline: Instant? {
        guard !isReleased, let hiddenSince else { return nil }
        let delay: Duration
        switch visibility {
        case .closed: delay = delays.closed
        case .occluded: delay = delays.occluded
        case .visible, .minimized, .appHidden: return nil
        }
        let due = hiddenSince.advanced(by: delay)
        guard let retryAt else { return due }
        return max(due, retryAt)
    }

    func isDue(at now: Instant) -> Bool {
        guard let releaseDeadline else { return false }
        return releaseDeadline <= now
    }

    /// Whether a memory pressure warning releases the window right away.
    var releasesOnMemoryPressure: Bool {
        !isReleased && visibility == .closed
    }

    /// The release was due but something blocks it: try again later.
    mutating func postpone(at now: Instant) {
        retryAt = now.advanced(by: delays.retry)
    }

    mutating func didRelease() {
        isReleased = true
        retryAt = nil
    }

    /// The views are back. The window is usually becoming visible; if it
    /// stays hidden, the delay starts over rather than releasing again at once.
    mutating func didRestore(at now: Instant) {
        isReleased = false
        retryAt = nil
        if hiddenSince != nil { hiddenSince = now }
    }
}
