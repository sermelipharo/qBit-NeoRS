import AppKit

/// Runs quitting (`TerminationGate`): prepares every quit request so open
/// sheets cannot stall it, answers `applicationShouldTerminate` with the lock
/// password and exit confirmation, and shuts the core down before replying.
///
/// AppKit's terminate machinery never runs inside a main-actor task: after
/// `.terminateLater` it waits in a nested run loop, and a main-actor job
/// waiting there keeps every other main-actor job (the reply) from running.
/// In-app requests and the reply therefore go through the run loop.
@MainActor
final class TerminationController {
    private weak var app: AppModel?
    private(set) var gate = TerminationGate()

    init(app: AppModel) {
        self.app = app
    }

    var isShuttingDown: Bool { gate.isShuttingDown }

    /// Wraps AppKit's quit Apple event handler; call once AppKit has installed it.
    func installQuitEventHandling() {
        QuitEventInterceptor.install { [weak self] isSystemSessionEnd in
            guard let self else { return .forward }
            switch prepareQuit(isSystemSessionEnd: isSystemSessionEnd) {
            case .proceed: return .forward
            case .cancelled: return .refuse
            case .alreadyQuitting: return .ignore
            }
        }
    }

    /// Quits from an in-app command (Quit menu items, the start-up error, the
    /// core's shutdown request, the completion action).
    func requestQuit() {
        Self.onMainRunLoop { [weak self] in
            guard let self, prepareQuit(isSystemSessionEnd: false) == .proceed else { return }
            NSApp.terminate(nil)
        }
    }

    enum PreparationResult {
        case proceed, cancelled, alreadyQuitting
    }

    /// Closes open sheets (asking the exit confirmation first when it applies)
    /// so AppKit goes on to `applicationShouldTerminate`.
    func prepareQuit(isSystemSessionEnd: Bool) -> PreparationResult {
        let windows = NSApp.windows
        switch gate.prepare(conditions(isSystemSessionEnd: isSystemSessionEnd), hasSheets: SheetDismissal.hasSheets(windows)) {
        case .proceed:
            return .proceed
        case .alreadyQuitting:
            return .alreadyQuitting
        case .refused:
            return .cancelled
        case .confirmExitFirst:
            guard gate.exitAnsweredWhilePreparing(askExitConfirmation()) else { return .cancelled }
        case .closeSheets:
            break
        }
        app?.dismissMainPresentations()
        SheetDismissal.closeAll(NSApp.windows)
        return .proceed
    }

    func shouldTerminate() -> NSApplication.TerminateReply {
        switch gate.shouldTerminate(conditions(isSystemSessionEnd: QuitReason.isSystemSessionEnd)) {
        case .terminateNow:
            return .terminateNow
        case .keepWaiting:
            return .terminateLater
        case .cancel:
            return .terminateCancel
        case .askPassword:
            Self.onMainRunLoop { [weak self] in self?.askPasswordThenFinish() }
            return .terminateLater
        case .confirmExit:
            guard gate.exitAnswered(askExitConfirmation()) == .shutDown else { return .terminateCancel }
            shutDown()
            return .terminateLater
        case .shutDown:
            shutDown()
            return .terminateLater
        }
    }

    // MARK: Steps

    private func conditions(isSystemSessionEnd: Bool) -> TerminationGate.Conditions {
        guard let app else { return TerminationGate.Conditions(isSystemSessionEnd: isSystemSessionEnd) }
        return TerminationGate.Conditions(
            isSnapshot: app.launch.isSnapshot,
            isLocked: app.lock.isLocked,
            isMainWindowShown: app.isMainWindowShown,
            isSystemSessionEnd: isSystemSessionEnd,
            confirmOnExit: app.gui.confirmOnExit,
            isTransferring: app.status.isTransferring,
            // The import sheet already asked: restarting for it skips the exit confirmation.
            isAutoExiting: CompletionActionController.isAutoExiting || app.isRestartingForImport
        )
    }

    /// The exit confirmation; "Always Yes" turns it off.
    private func askExitConfirmation() -> Bool {
        switch ExitConfirmation.ask() {
        case .no:
            return false
        case .always:
            app?.gui.confirmOnExit = false
            return true
        case .yes:
            return true
        }
    }

    /// Asks for the lock password (qBittorrent's Exit); unlocks on success.
    private func askPasswordThenFinish() {
        guard let password = QuitPasswordPrompt.ask() else {
            finishPasswordCheck(false)
            return
        }
        Task { [weak self] in
            guard let app = self?.app else { return }
            let allowed = await app.lock.unlock(with: password)
            if allowed { app.didUnlock() }
            Self.onMainRunLoop {
                if !allowed { QuitPasswordPrompt.showInvalid() }
                self?.finishPasswordCheck(allowed)
            }
        }
    }

    private func finishPasswordCheck(_ allowed: Bool) {
        if gate.passwordChecked(allowed) == .shutDown {
            shutDown()
        } else {
            NSApp.reply(toApplicationShouldTerminate: false)
        }
    }

    /// Stops refreshing, saves resume data and stops the core off the main
    /// thread, then lets AppKit finish terminating.
    private func shutDown() {
        app?.stopRefreshing()
        app?.widgets?.publishTermination()
        let client = app?.client
        let relaunches = app?.isRestartingForImport == true
        Task.detached {
            await AppModel.shutdown(client)
            Self.onMainRunLoop {
                // Only once the core is down: the new instance imports into a profile nobody uses.
                if relaunches { AppRelauncher.spawnAfterExit() }
                NSApp.reply(toApplicationShouldTerminate: true)
            }
        }
    }

    /// Runs `body` from the main run loop in the common modes (which include
    /// AppKit's modal-panel mode): unlike a main-actor task it also runs while
    /// a main-actor job is waiting in a nested run loop.
    nonisolated static func onMainRunLoop(_ body: @escaping @MainActor @Sendable () -> Void) {
        let runLoop = CFRunLoopGetMain()
        CFRunLoopPerformBlock(runLoop, CFRunLoopMode.commonModes.rawValue) {
            MainActor.assumeIsolated(body)
        }
        CFRunLoopWakeUp(runLoop)
    }
}
