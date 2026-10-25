import SwiftUI

/// Under the Add Torrent dialog's file list: how many files there are, how
/// much is selected, and the selected files by format, as many as fit.
struct AddTorrentFilesSummary: View {
    let dialog: AddTorrentModel

    var body: some View {
        let formats = dialog.formats
        HStack(spacing: Design.Spacing.medium) {
            Text(dialog.filesSummary)
                .layoutPriority(1)
            ViewThatFits(in: .horizontal) {
                ForEach((0...min(formats.count, 4)).reversed(), id: \.self) { shown in
                    chips(formats, shown: shown)
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .help(formats.map(\.description).joined(separator: "\n"))
        }
        .font(.callout)
        .foregroundStyle(.secondary)
        .monospacedDigit()
        .lineLimit(1)
    }

    private func chips(_ formats: [FileFormatShare], shown: Int) -> some View {
        HStack(spacing: Design.Spacing.small) {
            ForEach(formats.prefix(shown), id: \.format) { share in
                Text(share.description)
                    .padding(.horizontal, Design.Spacing.small)
                    .padding(.vertical, 1)
                    .background(.quaternary.opacity(0.6), in: .rect(cornerRadius: Design.Radius.small))
            }
            if formats.count > shown {
                Text(verbatim: shown == 0 ? String(localized: "\(formats.count) formats") : "+\(formats.count - shown)")
            }
        }
        .fixedSize()
    }
}
