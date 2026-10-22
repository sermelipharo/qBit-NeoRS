import SwiftUI

/// "Add Peers" (features.md §2.5.4): one `IPv4:port` / `[IPv6]:port` per line.
struct AddPeersSheet: View {
    let ids: [String]
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var error: String?

    var body: some View {
        DialogContainer(title: "Add Peers", width: Design.Window.dialogWidth, onConfirm: save) {
            Text("List of peers to add (one IP per line):")
            TrackerListEditor(text: $text, minHeight: 140)
                .accessibilityLabel(Text("Peers"))
            Text("Format: IPv4:port / [IPv6]:port")
                .font(.callout)
                .foregroundStyle(.secondary)
            if let error {
                Label(error, systemImage: Symbol.exclamationmarkTriangleFill)
                    .foregroundStyle(TorrentState.error.color)
            }
        }
    }

    private func save() {
        let lines = text.trimmingCharacters(in: .whitespacesAndNewlines).split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
        guard !lines.isEmpty, !(lines.count == 1 && lines[0].isEmpty) else {
            error = String(localized: "Please type at least one peer.")
            return
        }
        if let invalid = lines.first(where: { !PastedText.isPeerAddress($0) }) {
            error = String(localized: "The peer '\(invalid)' is invalid.")
            return
        }
        model.perform { try $0.addPeers(hashes: ids, peers: lines) }
        dismiss()
    }
}
