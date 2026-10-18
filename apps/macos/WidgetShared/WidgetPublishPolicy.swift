import Foundation

/// Rate limiting of snapshot writes and WidgetKit timeline reloads (pure;
/// times are monotonic seconds).
///
/// * A snapshot is built at most every `evaluationInterval` (user commands:
///   every `forcedSpacing`), and written only when its content changed.
/// * Reloads, which spend the WidgetKit budget, follow changes that matter
///   (status, counts, which torrents) at most every `structuralReloadInterval`,
///   and mere speed or progress changes at most every `volatileReloadInterval`.
/// * Every `heartbeatInterval` the snapshot is rewritten and reloaded even
///   when nothing changed, so a widget can tell a live app from a stale file.
struct WidgetPublishPolicy: Sendable {
    enum Decision: Equatable, Sendable {
        case none
        case write
        case writeAndReload
    }

    var evaluationInterval: TimeInterval = 15
    var structuralReloadInterval: TimeInterval = 30
    var volatileReloadInterval: TimeInterval = 180
    var heartbeatInterval: TimeInterval = 15 * 60
    var forcedSpacing: TimeInterval = 1

    private var lastEvaluation: TimeInterval?
    private var lastReload: TimeInterval?
    private var written: WidgetFingerprint?
    private var reloaded: WidgetFingerprint?

    init() {}

    /// Whether building a snapshot now could lead to a write.
    func shouldEvaluate(now: TimeInterval, forced: Bool) -> Bool {
        guard let lastEvaluation else { return true }
        return now - lastEvaluation >= (forced ? forcedSpacing : evaluationInterval)
    }

    /// Decides what to do with a freshly built snapshot and records the outcome.
    mutating func decide(_ fingerprint: WidgetFingerprint, now: TimeInterval, forced: Bool) -> Decision {
        lastEvaluation = now
        let decision = evaluate(fingerprint, now: now, forced: forced)
        switch decision {
        case .none:
            break
        case .write:
            written = fingerprint
        case .writeAndReload:
            lastReload = now
            written = fingerprint
            reloaded = fingerprint
        }
        return decision
    }

    /// Forgets everything, so the next snapshot is written and reloaded.
    mutating func reset() {
        self = Self()
    }

    private func evaluate(_ fingerprint: WidgetFingerprint, now: TimeInterval, forced: Bool) -> Decision {
        guard let lastReload, let reloaded else { return .writeAndReload }
        let sinceReload = now - lastReload
        let structuralInterval = forced ? forcedSpacing : structuralReloadInterval
        let volatileInterval = forced ? forcedSpacing : volatileReloadInterval
        if fingerprint.structural != reloaded.structural, sinceReload >= structuralInterval { return .writeAndReload }
        if fingerprint.volatile != reloaded.volatile, sinceReload >= volatileInterval { return .writeAndReload }
        if sinceReload >= heartbeatInterval { return .writeAndReload }
        return fingerprint != written ? .write : .none
    }
}
