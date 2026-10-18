import AppKit

/// Wraps AppKit's handler of the quit Apple event (Dock menu, Activity
/// Monitor, `NSRunningApplication.terminate()`, log out) so a quit is
/// prepared first (`TerminationController.prepareQuit`): AppKit would
/// otherwise drop it while a window shows a SwiftUI sheet. AppKit's own
/// handler then runs unchanged, so its reply to the sender still waits for
/// `applicationShouldTerminate` and the core shutdown.
@MainActor
enum QuitEventInterceptor {
    /// What to do with a quit event, given whether the system is ending the session.
    enum Decision: Sendable {
        case forward, refuse, ignore
    }

    nonisolated(unsafe) private static var appKitHandler: AEEventHandlerUPP?
    nonisolated(unsafe) private static var appKitRefcon: UnsafeMutableRawPointer?
    private static var decide: ((Bool) -> Decision)?

    /// Installs the wrapper over the handler AppKit registered; false when there is none yet.
    @discardableResult
    static func install(_ decide: @escaping @MainActor (_ isSystemSessionEnd: Bool) -> Decision) -> Bool {
        var handler: AEEventHandlerUPP?
        var refcon: UnsafeMutableRawPointer?
        let eventClass = AEEventClass(kCoreEventClass)
        let eventID = AEEventID(kAEQuitApplication)
        guard self.decide == nil,
              AEGetEventHandler(eventClass, eventID, &handler, &refcon, false) == noErr, let handler
        else { return false }
        appKitHandler = handler
        appKitRefcon = refcon
        self.decide = decide
        return AEInstallEventHandler(eventClass, eventID, quitHandler, nil, false) == noErr
    }

    /// Apple events are dispatched on the main thread.
    private static let quitHandler: AEEventHandlerUPP = { event, reply, _ in
        handle(event, reply)
    }

    nonisolated private static func handle(_ event: UnsafePointer<AppleEvent>?, _ reply: UnsafeMutablePointer<AppleEvent>?) -> OSErr {
        var isSessionEnd = false
        if let event, let copy = copyDescriptor(event) {
            isSessionEnd = QuitReason.isSystemSessionEnd(copy)
        }
        let decision = MainActor.assumeIsolated { decide?(isSessionEnd) ?? .forward }
        switch decision {
        case .forward:
            guard let appKitHandler else { return OSErr(truncatingIfNeeded: errAEEventNotHandled) }
            return appKitHandler(event, reply, appKitRefcon)
        case .refuse:
            return OSErr(truncatingIfNeeded: userCanceledErr)
        case .ignore:
            return OSErr(noErr)
        }
    }

    nonisolated private static func copyDescriptor(_ event: UnsafePointer<AppleEvent>) -> NSAppleEventDescriptor? {
        var copy = AEDesc()
        guard AEDuplicateDesc(event, &copy) == noErr else { return nil }
        return NSAppleEventDescriptor(aeDescNoCopy: &copy)
    }
}
