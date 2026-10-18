import AppKit

/// What keeps a hidden window's views alive although its release is due:
/// state that lives only in the view tree and would be lost or cut short.
@MainActor
enum WindowReleaseBlockers {
    /// Why `window` can't drop its views right now, or nil if it can.
    static func reason(_ window: NSWindow) -> String? {
        // Sheets (SwiftUI alerts and confirmation dialogs are sheets too).
        if window.attachedSheet != nil { return "sheet" }
        // Popovers and other windows attached to this one.
        if window.childWindows?.isEmpty == false { return "child window" }
        if NSApp.windows.contains(where: { $0.isVisible && $0 !== window && String(describing: type(of: $0)).contains("Popover") }) {
            return "popover"
        }
        // A text field or inline editor being edited.
        if let text = window.firstResponder as? NSTextView, text.isEditable { return "text editing" }
        // A drag, or a menu being tracked.
        if NSEvent.pressedMouseButtons != 0 { return "mouse down" }
        if RunLoop.current.currentMode == .eventTracking { return "event tracking" }
        return nil
    }
}
