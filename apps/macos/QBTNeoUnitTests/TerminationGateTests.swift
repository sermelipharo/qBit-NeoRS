import Testing

struct TerminationGateTests {
    private typealias Conditions = TerminationGate.Conditions

    private let plain = Conditions()
    private let transferring = Conditions(confirmOnExit: true, isTransferring: true)
    private let lockedHidden = Conditions(isLocked: true, isMainWindowShown: false)

    // MARK: Preparing (before AppKit sees the quit)

    @Test func withoutSheetsTheRequestGoesStraightToAppKit() {
        let gate = TerminationGate()
        #expect(gate.prepare(plain, hasSheets: false) == .proceed)
        #expect(gate.prepare(transferring, hasSheets: false) == .proceed)
    }

    @Test func openSheetsAreClosedFirst() {
        let gate = TerminationGate()
        #expect(gate.prepare(plain, hasSheets: true) == .closeSheets)
    }

    @Test func exitConfirmationIsAskedBeforeTheSheetsClose() {
        var gate = TerminationGate()
        #expect(gate.prepare(transferring, hasSheets: true) == .confirmExitFirst)
        // No keeps the sheets and the app.
        let result1 = gate.exitAnsweredWhilePreparing(false)
        #expect(!result1)
        #expect(gate.phase == .idle)
        #expect(gate.prepare(transferring, hasSheets: true) == .confirmExitFirst)
        // Yes: the delegate does not ask a second time.
        let result2 = gate.exitAnsweredWhilePreparing(true)
        #expect(result2)
        let result3 = gate.shouldTerminate(transferring)
        #expect(result3 == .shutDown)
        #expect(gate.isShuttingDown)
    }

    @Test func sessionEndAndAutoExitNeverAskBeforeClosingSheets() {
        let gate = TerminationGate()
        var sessionEnd = transferring
        sessionEnd.isSystemSessionEnd = true
        #expect(gate.prepare(sessionEnd, hasSheets: true) == .closeSheets)
        var autoExit = transferring
        autoExit.isAutoExiting = true
        #expect(gate.prepare(autoExit, hasSheets: true) == .closeSheets)
    }

    @Test func lockedHiddenAppLeavesTheQuestionsToTheDelegate() {
        var gate = TerminationGate()
        var conditions = lockedHidden
        conditions.confirmOnExit = true
        conditions.isTransferring = true
        #expect(gate.prepare(conditions, hasSheets: true) == .closeSheets)
        let result4 = gate.shouldTerminate(conditions)
        #expect(result4 == .askPassword)
    }

    @Test func requestsDuringAQuitAreIgnoredOrRefused() {
        var gate = TerminationGate()
        _ = gate.shouldTerminate(lockedHidden)
        #expect(gate.prepare(plain, hasSheets: true) == .refused)
        #expect(gate.prepare(plain, hasSheets: false) == .refused)
        _ = gate.passwordChecked(true)
        #expect(gate.prepare(plain, hasSheets: true) == .alreadyQuitting)
        #expect(gate.prepare(plain, hasSheets: false) == .alreadyQuitting)
    }

    // MARK: applicationShouldTerminate

    @Test func plainQuitShutsTheCoreDown() {
        var gate = TerminationGate()
        let result5 = gate.shouldTerminate(plain)
        #expect(result5 == .shutDown)
        #expect(gate.isShuttingDown)
        // A repeated request keeps waiting for the shutdown's reply.
        let result6 = gate.shouldTerminate(plain)
        #expect(result6 == .keepWaiting)
    }

    @Test func snapshotModeTerminatesAtOnce() {
        var gate = TerminationGate()
        let result7 = gate.shouldTerminate(Conditions(isSnapshot: true))
        #expect(result7 == .terminateNow)
    }

    @Test func activeTransfersAskForConfirmation() {
        var gate = TerminationGate()
        let result8 = gate.shouldTerminate(transferring)
        #expect(result8 == .confirmExit)
        let result9 = gate.exitAnswered(false)
        #expect(result9 == .cancel)
        #expect(gate.phase == .idle)
        let result10 = gate.shouldTerminate(transferring)
        #expect(result10 == .confirmExit)
        let result11 = gate.exitAnswered(true)
        #expect(result11 == .shutDown)
        #expect(gate.isShuttingDown)
    }

    @Test func confirmationIsSkippedWhenOffIdleOrAutoExitingOrSessionEnding() {
        for conditions in [
            Conditions(confirmOnExit: false, isTransferring: true),
            Conditions(confirmOnExit: true, isTransferring: false),
            Conditions(isSystemSessionEnd: true, confirmOnExit: true, isTransferring: true),
            Conditions(confirmOnExit: true, isTransferring: true, isAutoExiting: true),
        ] {
            var gate = TerminationGate()
            let result12 = gate.shouldTerminate(conditions)
            #expect(result12 == .shutDown)
        }
    }

    @Test func lockedHiddenAppAsksForThePassword() {
        var gate = TerminationGate()
        let result13 = gate.shouldTerminate(lockedHidden)
        #expect(result13 == .askPassword)
        // Another request while the prompt is up is cancelled.
        let result14 = gate.shouldTerminate(lockedHidden)
        #expect(result14 == .cancel)
        let result15 = gate.passwordChecked(false)
        #expect(result15 == .cancel)
        #expect(gate.phase == .idle)
        let result16 = gate.shouldTerminate(lockedHidden)
        #expect(result16 == .askPassword)
        let result17 = gate.passwordChecked(true)
        #expect(result17 == .shutDown)
        #expect(gate.isShuttingDown)
    }

    @Test func lockedAppWithVisibleWindowOrEndingSessionNeedsNoPassword() {
        var visible = TerminationGate()
        let result18 = visible.shouldTerminate(Conditions(isLocked: true, isMainWindowShown: true))
        #expect(result18 == .shutDown)
        var sessionEnd = TerminationGate()
        let result19 = sessionEnd.shouldTerminate(Conditions(isLocked: true, isMainWindowShown: false, isSystemSessionEnd: true))
        #expect(result19 == .shutDown)
    }

    @Test func lateAnswersAfterTheShutdownBeganChangeNothing() {
        var gate = TerminationGate()
        _ = gate.shouldTerminate(plain)
        let result20 = gate.passwordChecked(false)
        #expect(result20 == .keepWaiting)
        let result21 = gate.exitAnswered(false)
        #expect(result21 == .keepWaiting)
        #expect(gate.isShuttingDown)
    }
}
