import SwiftUI

/// The body of the import sheet for the current phase.
struct ImportPhaseView: View {
    let importModel: ImportModel

    var body: some View {
        Group { content }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder private var content: some View {
        let app = importModel.source.appName
        switch importModel.phase {
        case .loading:
            ProgressView("Reading \(app)'s data…")
        case .notFound:
            ContentUnavailableView {
                Label("No \(app) Data", systemImage: Symbol.tray)
            } description: {
                Text("\(app) has no torrents or settings on this Mac.")
            }
        case .sourceRunning:
            ContentUnavailableView {
                Label("Quit \(app) First", systemImage: Symbol.exclamationmarkTriangleFill)
            } description: {
                Text("\(app) saves the state of its torrents when it quits. Quit it, then check again. qBit NeoRS never quits it for you.")
            } actions: {
                Button("Check Again") { importModel.reload() }
            }
        case .failed(let message):
            ContentUnavailableView {
                Label("Can't Import from \(app)", systemImage: Symbol.exclamationmarkTriangle)
            } description: {
                Text(message)
            } actions: {
                Button("Try Again") { importModel.reload() }
            }
        case .ready(let plan):
            ImportPlanForm(importModel: importModel, plan: plan)
        }
    }
}
