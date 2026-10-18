import SwiftUI

/// Drops a window's view tree while nobody sees it and rebuilds it when the
/// window returns: closed for 2 minutes (at once under memory pressure) or
/// fully occluded for 10 minutes. SwiftUI keeps a closed `Window` or
/// `Settings` window with its whole hosting view alive until the app quits,
/// and a covered one anyway (docs/hidden-ui-memory.md; minimized and hidden
/// windows are left alone, see `WindowReleasePolicy`). Timing in
/// `WindowReleasePolicy`, the release and the snapshot shown while the views
/// come back in `WindowContentReleaser`.
///
/// Only for windows whose state lives in models outside the view tree: view
/// state (`@State`, scroll positions) starts over after a release unless it
/// is saved elsewhere.
struct ReleasedWhileHidden: ViewModifier {
    var onRelease: (@MainActor () -> Void)?
    @State private var releaser = WindowContentReleaser()
    /// The content's last size: the placeholder of a window sized to its
    /// content keeps it, so the window doesn't shrink while released and jump
    /// when it returns.
    @State private var size = CGSize.zero

    func body(content: Content) -> some View {
        Group {
            if releaser.isReleased {
                if releaser.isResizable {
                    // Stays resizable (zoom, Stage Manager, full screen) while released.
                    Color.clear
                } else {
                    Color.clear.frame(width: size.width, height: size.height)
                }
            } else {
                content
                    .onGeometryChange(for: CGSize.self, of: \.size) { size = $0 }
                    .onAppear { releaser.contentDidAppear() }
            }
        }
        .background {
            WindowLifecycleObserver(releaser: releaser, onRelease: onRelease)
        }
    }
}

extension View {
    /// See `ReleasedWhileHidden`.
    func releasedWhileHidden(onRelease: (@MainActor () -> Void)? = nil) -> some View {
        modifier(ReleasedWhileHidden(onRelease: onRelease))
    }
}
