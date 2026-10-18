import SwiftUI

/// "Import from qBittorrent… / Transmission…" (and the first-launch offer):
/// the dry run's summary, what doesn't carry over, then Import and Restart.
struct ImportSheet: View {
    @State private var importModel: ImportModel
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    static let size = CGSize(width: 560, height: 720)

    init(request: ImportSheetRequest, client: any ImportClient, profileDirectory: String) {
        _importModel = State(initialValue: ImportModel(request: request, client: client, profileDirectory: profileDirectory))
    }

    var body: some View {
        @Bindable var importModel = importModel
        let app = importModel.source.appName
        VStack(alignment: .leading, spacing: Design.Spacing.medium) {
            HStack(alignment: .top, spacing: Design.Spacing.medium) {
                Image(systemName: Symbol.trayAndArrowDown)
                    .font(.largeTitle)
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Design.Spacing.tight) {
                    Text(importModel.isOffer ? "Import Your Torrents" : "Import from \(app)")
                        .font(.headline)
                    Text(importModel.isOffer
                        ? "qBit NeoRS found \(app) on this Mac. You can bring over its torrents and settings."
                        : "Brings over \(app)'s torrents, their progress and its settings.")
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if importModel.sources.count > 1 {
                Picker("Import from:", selection: $importModel.source) {
                    ForEach(importModel.sources, id: \.self) { Text($0.appName).tag($0) }
                }
                .pickerStyle(.segmented)
            }
            ImportPhaseView(importModel: importModel)
                .frame(maxHeight: .infinity, alignment: .top)
            Text("qBit NeoRS restarts to import. Its profile is backed up first. \(app)'s files are only read, and downloaded files are never moved.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button(importModel.isOffer ? "Not Now" : "Cancel", role: .cancel) {
                    importModel.cancel()
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                Button("Import and Restart", action: confirm)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!importModel.canImport)
            }
        }
        .padding(Design.Spacing.xLarge)
        .frame(width: Self.size.width, height: Self.size.height)
        .task { importModel.reload() }
    }

    private func confirm() {
        guard importModel.confirm() else { return }
        dismiss()
        model.restartForImport()
    }
}
