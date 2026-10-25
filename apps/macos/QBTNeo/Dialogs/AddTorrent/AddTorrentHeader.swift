import SwiftUI

/// Top of the Add Torrent dialog: the content kind, the torrent's name with
/// the tags parsed from it, the size to download and the free space, and the
/// torrent information (date, hashes, comment) behind an info button.
struct AddTorrentHeader: View {
    let dialog: AddTorrentModel
    @Environment(AppModel.self) private var model
    @State private var showsInfo = false

    var body: some View {
        let name = dialog.metadata?.name ?? dialog.title
        let parsed = model.names.parsed(name)
        let kind = model.names.contentKind(name: name, category: dialog.category)
        HStack(spacing: Design.Spacing.regular) {
            Image(systemName: kind.symbol)
                .font(.system(size: 20, weight: .medium))
                .foregroundStyle(.tint)
                .frame(width: 40, height: 40)
                .background(.tint.opacity(0.14), in: .rect(cornerRadius: Design.Radius.large))
                .accessibilityLabel(Text(kind.title))
            VStack(alignment: .leading, spacing: Design.Spacing.tight) {
                Text(parsed?.title ?? name)
                    .font(.title3.weight(.semibold))
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .help(name)
                HStack(spacing: Design.Spacing.small) {
                    ForEach(Array((parsed?.chips ?? []).prefix(5).enumerated()), id: \.offset) { _, chip in
                        NameChipLabel(chip)
                    }
                    status
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button("Torrent Information", systemImage: Symbol.infoCircle) { showsInfo.toggle() }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .font(.title3)
                .foregroundStyle(.secondary)
                .help("Torrent information")
                .popover(isPresented: $showsInfo, arrowEdge: .bottom) {
                    AddTorrentInfoPanel(dialog: dialog)
                        .padding(Design.Spacing.medium)
                        .frame(width: 420)
                }
        }
    }

    @ViewBuilder private var status: some View {
        switch dialog.phase {
        case .failed(let message):
            Label(message, systemImage: Symbol.exclamationmarkTriangle)
                .foregroundStyle(.red)
                .lineLimit(1)
                .help(message)
        default:
            if dialog.hasMetadata {
                Text(dialog.sizeSummary)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else {
                ProgressView().controlSize(.mini)
                Text("Retrieving metadata…")
                    .foregroundStyle(.secondary)
            }
        }
    }
}
