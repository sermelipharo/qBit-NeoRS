import Foundation
import Testing

struct WindowReleasePolicyTests {
    private typealias Policy = WindowReleasePolicy<ContinuousClock.Instant>
    private let start = ContinuousClock.now

    private func at(_ seconds: Int) -> ContinuousClock.Instant {
        start + .seconds(seconds)
    }

    @Test func visibleWindowIsNeverDue() {
        var policy = Policy()
        policy.update(.visible, at: at(0))
        #expect(policy.releaseDeadline == nil)
        #expect(!policy.isDue(at: at(100_000)))
        #expect(!policy.releasesOnMemoryPressure)
    }

    @Test func closedWindowIsReleasedAfterTwoMinutes() {
        var policy = Policy()
        policy.update(.closed, at: at(10))
        #expect(policy.releaseDeadline == at(130))
        #expect(!policy.isDue(at: at(129)))
        #expect(policy.isDue(at: at(130)))
        #expect(policy.releasesOnMemoryPressure)
    }

    @Test func occludedWindowIsReleasedAfterTenMinutes() {
        var policy = Policy()
        policy.update(.occluded, at: at(0))
        #expect(policy.releaseDeadline == at(600))
        #expect(!policy.isDue(at: at(599)))
        #expect(policy.isDue(at: at(600)))
        // It may be uncovered at any moment, so memory pressure leaves it alone.
        #expect(!policy.releasesOnMemoryPressure)
    }

    @Test(arguments: [WindowVisibility.minimized, .appHidden])
    func minimizedOrHiddenWindowKeepsItsViews(_ state: WindowVisibility) {
        var policy = Policy()
        policy.update(state, at: at(0))
        #expect(policy.releaseDeadline == nil)
        #expect(!policy.isDue(at: at(100_000)))
        #expect(!policy.releasesOnMemoryPressure)
    }

    @Test func visibilityInBetweenStartsOver() {
        var policy = Policy()
        policy.update(.occluded, at: at(0))
        policy.update(.visible, at: at(590))
        #expect(policy.releaseDeadline == nil)
        policy.update(.occluded, at: at(595))
        #expect(policy.releaseDeadline == at(1_195))
    }

    @Test func changingHiddenStateKeepsTheStart() {
        var policy = Policy()
        policy.update(.occluded, at: at(0))
        policy.update(.minimized, at: at(60))
        #expect(policy.releaseDeadline == nil)
        // Back to occluded without being seen (the app was hidden and shown again behind other windows).
        policy.update(.occluded, at: at(700))
        #expect(policy.isDue(at: at(700)))
    }

    @Test func closingAnOccludedWindowUsesTheClosedDelay() {
        var policy = Policy()
        policy.update(.occluded, at: at(0))
        policy.update(.closed, at: at(30))
        #expect(policy.releaseDeadline == at(120))
    }

    @Test func blockedReleaseIsRetriedLater() {
        var policy = Policy()
        policy.update(.closed, at: at(0))
        #expect(policy.isDue(at: at(120)))
        policy.postpone(at: at(120))
        #expect(policy.releaseDeadline == at(150))
        #expect(!policy.isDue(at: at(149)))
        #expect(policy.isDue(at: at(150)))
        // Showing the window clears the retry.
        policy.update(.visible, at: at(140))
        policy.update(.closed, at: at(141))
        #expect(policy.releaseDeadline == at(261))
    }

    @Test func releasedWindowIsNotDueAgainUntilRestored() {
        var policy = Policy()
        policy.update(.closed, at: at(0))
        policy.didRelease()
        #expect(policy.releaseDeadline == nil)
        #expect(!policy.releasesOnMemoryPressure)
        policy.update(.occluded, at: at(5_000))
        #expect(policy.releaseDeadline == nil)
    }

    @Test func restoringWhileStillHiddenStartsTheDelayOver() {
        var policy = Policy()
        policy.update(.occluded, at: at(0))
        policy.didRelease()
        // Becoming key or main starts the rebuild before the window reports itself visible.
        policy.didRestore(at: at(900))
        #expect(policy.releaseDeadline == at(1_500))
        policy.update(.visible, at: at(901))
        #expect(policy.releaseDeadline == nil)
    }

    @Test func customDelays() {
        var policy = Policy(delays: .init(closed: .seconds(5), occluded: .seconds(8), retry: .seconds(2)))
        policy.update(.closed, at: at(0))
        #expect(policy.releaseDeadline == at(5))
        policy.update(.occluded, at: at(1))
        #expect(policy.releaseDeadline == at(8))
        policy.postpone(at: at(8))
        #expect(policy.releaseDeadline == at(10))
    }

    @Test func onlyOccludedWindowsUseASnapshot() {
        #expect(WindowVisibility.occluded.restoresSnapshot)
        #expect(!WindowVisibility.minimized.restoresSnapshot)
        #expect(!WindowVisibility.appHidden.restoresSnapshot)
        #expect(!WindowVisibility.closed.restoresSnapshot)
        #expect(!WindowVisibility.visible.restoresSnapshot)
    }
}
