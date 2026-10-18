import AppKit
import Observation
import os

/// Drops a window's views while nobody sees it and brings them back when it
/// returns (`ReleasedWhileHidden`, timing rules in `WindowReleasePolicy`).
/// A reference type with its own timer, because SwiftUI stops a hidden
/// window's `.task` and `.onChange` work: view-lifecycle tools never fire
/// while the window is closed, which is exactly when this needs to.
///
/// A window that returns as it was (occluded, see `WindowVisibility`) is
/// photographed before the release (`WindowSnapshot`); the picture covers the
/// window from the earliest sign of its return until the rebuilt views have
/// drawn, then fades out and is freed.
@Observable @MainActor
final class WindowContentReleaser {
    private(set) var isReleased = false
    /// Whether the user can resize the window (not sized to its content).
    private(set) var isResizable = false
    @ObservationIgnored var onRelease: (@MainActor () -> Void)?
    @ObservationIgnored private var policy = WindowReleasePolicy<ContinuousClock.Instant>(delays: WindowContentReleaser.delays)
    @ObservationIgnored private weak var window: NSWindow?
    @ObservationIgnored private var timer: Task<Void, Never>?
    @ObservationIgnored private var capture: Task<Void, Never>?
    @ObservationIgnored private var pressureObserver: (any NSObjectProtocol)?
    @ObservationIgnored private var snapshot: WindowSnapshot?
    @ObservationIgnored private let overlay = WindowSnapshotOverlay()
    @ObservationIgnored private var restoreStarted: ContinuousClock.Instant?

    private static let log = Logger(subsystem: "mk.lex.qbitneors", category: "WindowRelease")

    /// The release delays. Debug builds accept `QBT_WINDOW_RELEASE_DELAYS=<closed>,<occluded>`
    /// (seconds) to test without waiting minutes.
    private static let delays: WindowReleasePolicy<ContinuousClock.Instant>.Delays = {
        var delays = WindowReleasePolicy<ContinuousClock.Instant>.Delays()
        #if DEBUG
        let parts = (ProcessInfo.processInfo.environment["QBT_WINDOW_RELEASE_DELAYS"] ?? "").split(separator: ",").compactMap { Int($0) }
        if parts.count == 2 {
            delays.closed = .seconds(parts[0])
            delays.occluded = .seconds(parts[1])
            delays.retry = .seconds(min(parts[0], 30))
        }
        #endif
        return delays
    }()

    // MARK: Window events

    func attach(_ window: NSWindow) {
        self.window = window
    }

    func visibilityChanged(_ visibility: WindowVisibility) {
        guard visibility != policy.visibility else { return }
        policy.update(visibility, at: .now)
        if visibility == .visible {
            capture?.cancel()
            capture = nil
            if isReleased { restore() }
        }
        schedule()
    }

    /// The window is about to be shown or has just been: rebuild now.
    func windowWillShow() {
        capture?.cancel()
        capture = nil
        if isReleased { restore() }
    }

    /// The rebuilt content appeared: remove the picture once it has drawn.
    func contentDidAppear() {
        guard let started = restoreStarted, let window else { return }
        restoreStarted = nil
        // Lay out and draw the new views now rather than at the next display
        // pass, then give the window server a frame or two to show them.
        DispatchQueue.main.async { [weak self] in
            window.contentView?.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            CATransaction.flush()
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(50)) {
                guard let self, !self.isReleased else { return }
                let fade = window.isVisible && window.occlusionState.contains(.visible)
                self.overlay.remove(fade: fade)
                self.snapshot = nil
                Self.log.debug("restored: overlay removed \(Int((ContinuousClock.now - started) / .milliseconds(1)), privacy: .public) ms after restore")
            }
        }
    }

    // MARK: Releasing

    private func schedule() {
        timer?.cancel()
        timer = nil
        stopPressureObserver()
        guard let deadline = policy.releaseDeadline else { return }
        timer = Task { [weak self] in
            try? await Task.sleep(until: deadline, clock: .continuous)
            guard !Task.isCancelled else { return }
            self?.attemptRelease(underPressure: false)
        }
        if policy.releasesOnMemoryPressure {
            pressureObserver = NotificationCenter.default.addObserver(
                forName: MemoryPressureMonitor.warningNotification, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.attemptRelease(underPressure: true) }
            }
        }
    }

    private func attemptRelease(underPressure: Bool) {
        guard !isReleased, capture == nil, underPressure || policy.isDue(at: .now) else { return }
        let visibility = policy.visibility
        guard visibility != .visible else { return }
        if let window, visibility != .closed, let reason = WindowReleaseBlockers.reason(window) {
            Self.log.debug("release postponed: \(reason, privacy: .public)")
            policy.postpone(at: .now)
            schedule()
            return
        }
        guard visibility.restoresSnapshot, let window else {
            release(standby: nil)
            return
        }
        // An occluded window can be uncovered before the app is told (60–80 ms
        // later), so a point-resolution copy stays in the window meanwhile;
        // the full picture replaces it as soon as the app hears of it.
        let standbyScale = 1 / window.backingScaleFactor
        capture = Task { [weak self] in
            let snapshot = await WindowSnapshot.capture(window, standbyScale: standbyScale)
            guard let self, !Task.isCancelled else { return }
            capture = nil
            // Shown again, or hidden differently, while capturing.
            guard policy.visibility == visibility, !isReleased else { return schedule() }
            guard let snapshot, snapshot.fits(window) else {
                if underPressure {
                    release(standby: nil)
                } else {
                    policy.postpone(at: .now)
                    schedule()
                }
                return
            }
            self.snapshot = snapshot
            release(standby: snapshot.standby)
        }
    }

    private func release(standby: CGImage?) {
        // The picture goes in before the views go away, so no frame shows the window empty.
        if let standby, let window { _ = overlay.show(standby, in: window) }
        isResizable = window?.styleMask.contains(.resizable) ?? false
        isReleased = true
        policy.didRelease()
        schedule()
        onRelease?()
        let bytes = snapshot?.png.count ?? 0
        Self.log.debug("released (\(String(describing: self.policy.visibility), privacy: .public), snapshot \(bytes, privacy: .public) bytes)")
        flush()
    }

    /// A window that isn't displayed doesn't lay out, so the AppKit views
    /// SwiftUI removed (table, toolbar, layers) would stay alive until it is
    /// shown again: run that pass now.
    private func flush() {
        DispatchQueue.main.async { [weak self] in
            guard let self, isReleased, let window else { return }
            window.contentView?.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
        }
    }

    private func restore() {
        let started = ContinuousClock.now
        if let window, let snapshot, snapshot.fits(window), let image = snapshot.decoded(), overlay.show(image, in: window) {
            restoreStarted = started
            // In case the content never reports itself (it should within a fraction of a second).
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
                guard let self, restoreStarted == started else { return }
                restoreStarted = nil
                overlay.remove(fade: true)
                self.snapshot = nil
            }
        } else {
            overlay.remove()
            snapshot = nil
            restoreStarted = nil
        }
        isReleased = false
        policy.didRestore(at: started)
        schedule()
        Self.log.debug("restoring (\(self.restoreStarted == nil ? "no snapshot" : "snapshot", privacy: .public))")
    }

    private func stopPressureObserver() {
        if let pressureObserver { NotificationCenter.default.removeObserver(pressureObserver) }
        pressureObserver = nil
    }

    isolated deinit {
        timer?.cancel()
        capture?.cancel()
        stopPressureObserver()
    }
}
