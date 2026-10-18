import AppKit
import SwiftUI

/// Hosts `SidebarController`.
struct SidebarOutline: NSViewRepresentable {
    let model: AppModel

    func makeCoordinator() -> SidebarController {
        SidebarController(model: model)
    }

    func makeNSView(context: Context) -> NSScrollView {
        if context.environment.isSnapshotRendering {
            context.coordinator.outline.backgroundColor = .windowBackgroundColor
            context.coordinator.scrollView.drawsBackground = true
        }
        context.coordinator.start()
        return context.coordinator.scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}

    static func dismantleNSView(_ nsView: NSScrollView, coordinator: SidebarController) {
        coordinator.stop()
    }
}
