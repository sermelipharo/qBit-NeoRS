import AppKit
import SwiftUI

/// Hosts the AppKit cards view (`TransferCardsController`) in the main window.
struct TransferCardsView: NSViewRepresentable {
    let model: AppModel

    func makeCoordinator() -> TransferCardsController {
        TransferCardsController(model: model)
    }

    func makeNSView(context: Context) -> NSScrollView {
        context.coordinator.start()
        context.coordinator.restoreScrollPosition()
        return context.coordinator.scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}

    static func dismantleNSView(_ nsView: NSScrollView, coordinator: TransferCardsController) {
        coordinator.stop()
    }
}
