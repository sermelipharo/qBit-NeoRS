import AppKit
import SwiftUI

/// Tells a `WindowContentReleaser` how visible its window is (closed,
/// minimized, app hidden, occluded, visible) and gives it the earliest sign
/// that the window is about to be seen again: the app unhiding, the window
/// being ordered in or deminiaturized (before the animation starts), becoming
/// key or main (synchronous with ordering in, before it draws), and occlusion
/// as the fallback for windows ordered in without becoming key.
struct WindowLifecycleObserver: NSViewRepresentable {
    let releaser: WindowContentReleaser
    var onRelease: (@MainActor () -> Void)?

    func makeNSView(context: Context) -> ObserverView {
        let view = ObserverView()
        view.releaser = releaser
        releaser.onRelease = onRelease
        return view
    }

    func updateNSView(_ nsView: ObserverView, context: Context) {
        nsView.releaser = releaser
        releaser.onRelease = onRelease
    }

    final class ObserverView: NSView {
        /// AppKit posts this (undocumented) right before a window is ordered
        /// on screen, including at the start of the deminiaturize animation,
        /// which otherwise reports nothing until it has finished. If it ever
        /// goes away, the documented notifications below still restore the
        /// window, just later.
        private static let willBecomeVisible = Notification.Name("_NSWindowWillBecomeVisible")

        weak var releaser: WindowContentReleaser?
        private var observers: [any NSObjectProtocol] = []
        private var visibility = WindowVisibility.visible

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            for observer in observers { NotificationCenter.default.removeObserver(observer) }
            observers.removeAll()
            guard let window else { return }
            releaser?.attach(window)
            let center = NotificationCenter.default
            // Changes that mean the window is being shown (it may still count as occluded for a moment).
            let shown: [Notification.Name] = [
                NSWindow.didDeminiaturizeNotification, NSWindow.didBecomeKeyNotification, NSWindow.didBecomeMainNotification,
            ]
            for name in shown {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.update(showing: true) }
                })
            }
            for name in [NSWindow.didMiniaturizeNotification, NSWindow.didChangeOcclusionStateNotification] {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.update(showing: false) }
                })
            }
            observers.append(center.addObserver(forName: NSWindow.willCloseNotification, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.report(.closed) }
            })
            observers.append(center.addObserver(forName: Self.willBecomeVisible, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.releaser?.windowWillShow() }
            })
            observers.append(center.addObserver(forName: NSApplication.didHideNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.appDidHide() }
            })
            observers.append(center.addObserver(forName: NSApplication.willUnhideNotification, object: nil, queue: .main) { [weak self] _ in
                // Before the windows are ordered back in.
                MainActor.assumeIsolated { if self?.visibility == .appHidden { self?.releaser?.windowWillShow() } }
            })
            observers.append(center.addObserver(forName: NSApplication.didUnhideNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.update(showing: false) }
            })
            update(showing: false)
        }

        private func appDidHide() {
            // A closed or minimized window stays what it was; only shown windows are hidden with the app.
            if visibility == .visible || visibility == .occluded { report(.appHidden) }
        }

        private func update(showing: Bool) {
            guard let window else { return }
            let next: WindowVisibility
            if window.isMiniaturized {
                next = .minimized
            } else if NSApp.isHidden {
                next = visibility == .closed ? .closed : .appHidden
            } else if !window.isVisible {
                next = .closed
            } else if !window.occlusionState.contains(.visible) {
                next = .occluded
            } else {
                next = .visible
            }
            if showing, window.isVisible, next != .minimized { releaser?.windowWillShow() }
            report(next)
        }

        private func report(_ next: WindowVisibility) {
            visibility = next
            releaser?.visibilityChanged(next)
        }
    }
}
