import AppKit
import QuartzCore

/// Shows a `WindowSnapshot` over a whole window (title bar and toolbar
/// included: their SwiftUI items go with the released views). A plain layer
/// on the window's frame view rather than a subview, which AppKit doesn't
/// allow there. The traffic-light buttons stay uncovered so they react to
/// the pointer and key state as usual. Removed at once when the window's size
/// or backing scale changes, so a picture is never shown stretched.
@MainActor
final class WindowSnapshotOverlay {
    private weak var window: NSWindow?
    private var layer: CALayer?
    private var observers: [any NSObjectProtocol] = []

    var isShown: Bool { layer != nil }

    func show(_ image: CGImage, in window: NSWindow) -> Bool {
        guard let frameView = window.contentView?.superview, let host = frameView.layer else { return false }
        let layer = self.layer ?? CALayer()
        layer.removeAllAnimations()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer.frame = host.bounds
        layer.contents = image
        layer.contentsGravity = .resize
        layer.contentsScale = window.backingScaleFactor
        layer.opacity = 1
        layer.zPosition = 10_000
        layer.mask = Self.buttonCutout(in: window, frameView: frameView)
        if layer.superlayer !== host { host.addSublayer(layer) }
        CATransaction.commit()
        self.layer = layer
        if self.window !== window { observe(window) }
        return true
    }

    /// Removes the picture; `fade` cross-fades to the content below.
    func remove(fade: Bool = false) {
        stopObserving()
        guard let layer else { return }
        self.layer = nil
        guard fade else {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            layer.removeFromSuperlayer()
            layer.contents = nil
            CATransaction.commit()
            return
        }
        CATransaction.begin()
        CATransaction.setAnimationDuration(0.15)
        CATransaction.setCompletionBlock {
            layer.removeFromSuperlayer()
            layer.contents = nil
        }
        layer.opacity = 0
        CATransaction.commit()
    }

    /// A mask that leaves the close/minimize/zoom buttons visible.
    private static func buttonCutout(in window: NSWindow, frameView: NSView) -> CALayer? {
        let buttons = [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton].compactMap { window.standardWindowButton($0) }
        let rects = buttons.filter { !$0.isHidden }.map { $0.convert($0.bounds, to: frameView).insetBy(dx: -3, dy: -3) }
        guard let first = rects.first else { return nil }
        let hole = rects.dropFirst().reduce(first) { $0.union($1) }
        let path = CGMutablePath()
        path.addRect(frameView.bounds)
        path.addRect(hole)
        let mask = CAShapeLayer()
        mask.frame = frameView.bounds
        mask.path = path
        mask.fillRule = .evenOdd
        return mask
    }

    private func observe(_ window: NSWindow) {
        stopObserving()
        self.window = window
        let names: [Notification.Name] = [
            NSWindow.didResizeNotification, NSWindow.didChangeBackingPropertiesNotification, NSWindow.willEnterFullScreenNotification,
        ]
        for name in names {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.remove() }
            })
        }
    }

    private func stopObserving() {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers.removeAll()
        window = nil
    }

    isolated deinit {
        remove()
    }
}
