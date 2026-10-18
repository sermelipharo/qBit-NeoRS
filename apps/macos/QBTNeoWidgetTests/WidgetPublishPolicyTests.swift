import Foundation
import Testing

struct WidgetPublishPolicyTests {
    private let base = WidgetFingerprint(structural: 1, volatile: 1)

    @Test func firstSnapshotIsWrittenAndReloaded() {
        var policy = WidgetPublishPolicy()
        #expect(policy.shouldEvaluate(now: 0, forced: false))
        #expect(policy.decide(base, now: 0, forced: false) == .writeAndReload)
    }

    @Test func evaluationIsSpaced() {
        var policy = WidgetPublishPolicy()
        _ = policy.decide(base, now: 100, forced: false)
        #expect(!policy.shouldEvaluate(now: 110, forced: false))
        #expect(policy.shouldEvaluate(now: 115, forced: false))
        #expect(!policy.shouldEvaluate(now: 100.5, forced: true))
        #expect(policy.shouldEvaluate(now: 101, forced: true))
    }

    @Test func unchangedContentDoesNothingUntilTheHeartbeat() {
        var policy = WidgetPublishPolicy()
        _ = policy.decide(base, now: 0, forced: false)
        #expect(policy.decide(base, now: 15, forced: false) == .none)
        #expect(policy.decide(base, now: 899, forced: false) == .none)
        #expect(policy.decide(base, now: 900, forced: false) == .writeAndReload)
    }

    @Test func speedChangesAreWrittenButReloadedRarely() {
        var policy = WidgetPublishPolicy()
        _ = policy.decide(base, now: 0, forced: false)
        let faster = WidgetFingerprint(structural: 1, volatile: 2)
        #expect(policy.decide(faster, now: 15, forced: false) == .write)
        #expect(policy.decide(faster, now: 30, forced: false) == .none)
        #expect(policy.decide(faster, now: 180, forced: false) == .writeAndReload)
        #expect(policy.decide(faster, now: 195, forced: false) == .none)
    }

    @Test func structuralChangesReloadSooner() {
        var policy = WidgetPublishPolicy()
        _ = policy.decide(base, now: 0, forced: false)
        let other = WidgetFingerprint(structural: 2, volatile: 1)
        #expect(policy.decide(other, now: 15, forced: false) == .write)
        // Still pending: reloaded once the interval has passed, even though
        // the file already holds this content.
        #expect(policy.decide(other, now: 30, forced: false) == .writeAndReload)
    }

    @Test func forcedUpdatesSkipTheIntervals() {
        var policy = WidgetPublishPolicy()
        _ = policy.decide(base, now: 0, forced: false)
        let other = WidgetFingerprint(structural: 2, volatile: 1)
        #expect(policy.decide(other, now: 0.5, forced: true) == .write)
        #expect(policy.decide(other, now: 1.5, forced: true) == .writeAndReload)
        #expect(policy.decide(other, now: 3, forced: true) == .none)
    }

    @Test func resetStartsOver() {
        var policy = WidgetPublishPolicy()
        _ = policy.decide(base, now: 0, forced: false)
        policy.reset()
        #expect(policy.shouldEvaluate(now: 1, forced: false))
        #expect(policy.decide(base, now: 1, forced: false) == .writeAndReload)
    }
}
