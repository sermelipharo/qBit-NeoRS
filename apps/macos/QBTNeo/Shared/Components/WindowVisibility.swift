/// Whether a window can be seen, and if not, why. Drives when its view tree
/// is released (`WindowReleasePolicy`).
enum WindowVisibility: Sendable, Equatable {
    /// On screen and at least partly visible.
    case visible
    /// On screen, but completely covered by other windows (or on another Space).
    case occluded
    /// Minimized to the Dock.
    case minimized
    /// The app is hidden (⌘H).
    case appHidden
    /// Closed (ordered out; SwiftUI keeps the window object).
    case closed

    /// A window released in this state comes back as it was, so its rebuild
    /// is covered with a snapshot (`WindowSnapshot`). A closed window opens
    /// like a new one and needs none.
    var restoresSnapshot: Bool { self == .occluded }
}
