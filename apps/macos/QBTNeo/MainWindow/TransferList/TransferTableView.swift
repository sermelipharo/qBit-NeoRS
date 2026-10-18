import AppKit
import SwiftUI

/// Hosts the AppKit transfer list (`TransferTableController`) in the main window.
struct TransferTableView: NSViewRepresentable {
    let model: AppModel

    func makeCoordinator() -> TransferTableController {
        TransferTableController(model: model)
    }

    func makeNSView(context: Context) -> NSScrollView {
        context.coordinator.start()
        context.coordinator.restoreScrollPosition()
        return context.coordinator.scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}

    static func dismantleNSView(_ nsView: NSScrollView, coordinator: TransferTableController) {
        coordinator.stop()
    }
}
