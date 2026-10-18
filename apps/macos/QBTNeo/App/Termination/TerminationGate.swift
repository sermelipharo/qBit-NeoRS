/// The steps of quitting, as pure state (unit-tested); `TerminationController`
/// runs the dialogs, closes the sheets and answers AppKit.
///
/// AppKit abandons a quit, without asking the delegate, while a window shows
/// a SwiftUI sheet, so every quit request is *prepared* first: open sheets are
/// closed before AppKit sees it. When the exit confirmation applies, it is
/// asked before the sheets close, so No keeps them and their input.
///
/// Then `applicationShouldTerminate`: the lock password (locked app with a
/// hidden window), the exit confirmation (transfers active), then the core
/// shutdown. The system ending the session skips both questions. Once the
/// shutdown has begun, later requests change nothing.
struct TerminationGate: Equatable, Sendable {
    struct Conditions: Equatable, Sendable {
        var isSnapshot = false
        var isLocked = false
        var isMainWindowShown = true
        var isSystemSessionEnd = false
        var confirmOnExit = false
        var isTransferring = false
        var isAutoExiting = false
    }

    enum Phase: Equatable, Sendable {
        case idle
        /// The exit was confirmed while preparing; the delegate does not ask again.
        case confirmed
        case askingPassword
        case shuttingDown
    }

    /// What preparing a quit request does.
    enum Preparation: Equatable, Sendable {
        /// Nothing to close: hand the request to AppKit.
        case proceed
        /// Close the open sheets, then hand the request to AppKit.
        case closeSheets
        /// Ask the exit confirmation first (sheets stay open on No).
        case confirmExitFirst
        /// A quit is under way already (shutdown running).
        case alreadyQuitting
        /// The lock password is being asked; this request is refused.
        case refused
    }

    /// What `applicationShouldTerminate` does.
    enum Step: Equatable, Sendable {
        case terminateNow
        case askPassword
        case confirmExit
        case shutDown
        case cancel
        /// The shutdown is running; keep waiting for its reply.
        case keepWaiting
    }

    private(set) var phase = Phase.idle

    var isShuttingDown: Bool { phase == .shuttingDown }

    func prepare(_ conditions: Conditions, hasSheets: Bool) -> Preparation {
        switch phase {
        case .shuttingDown: return .alreadyQuitting
        case .askingPassword: return .refused
        case .idle, .confirmed: break
        }
        guard hasSheets else { return .proceed }
        if phase == .idle, !needsPassword(conditions), needsExitConfirmation(conditions) { return .confirmExitFirst }
        return .closeSheets
    }

    /// The answer to the exit confirmation asked while preparing: true continues with `.closeSheets`.
    mutating func exitAnsweredWhilePreparing(_ quit: Bool) -> Bool {
        phase = quit ? .confirmed : .idle
        return quit
    }

    mutating func shouldTerminate(_ conditions: Conditions) -> Step {
        switch phase {
        case .shuttingDown: return .keepWaiting
        case .askingPassword: return .cancel
        case .idle, .confirmed: break
        }
        if conditions.isSnapshot { return .terminateNow }
        if needsPassword(conditions) {
            phase = .askingPassword
            return .askPassword
        }
        if phase == .idle, needsExitConfirmation(conditions) { return .confirmExit }
        phase = .shuttingDown
        return .shutDown
    }

    mutating func passwordChecked(_ allowed: Bool) -> Step {
        guard phase == .askingPassword else { return phase == .shuttingDown ? .keepWaiting : .cancel }
        phase = allowed ? .shuttingDown : .idle
        return allowed ? .shutDown : .cancel
    }

    mutating func exitAnswered(_ quit: Bool) -> Step {
        guard phase == .idle || phase == .confirmed else { return phase == .shuttingDown ? .keepWaiting : .cancel }
        phase = quit ? .shuttingDown : .idle
        return quit ? .shutDown : .cancel
    }

    private func needsPassword(_ conditions: Conditions) -> Bool {
        conditions.isLocked && !conditions.isMainWindowShown && !conditions.isSystemSessionEnd
    }

    private func needsExitConfirmation(_ conditions: Conditions) -> Bool {
        conditions.confirmOnExit && conditions.isTransferring && !conditions.isAutoExiting && !conditions.isSystemSessionEnd
    }
}
