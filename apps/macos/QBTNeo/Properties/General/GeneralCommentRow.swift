import SwiftUI

/// The torrent comment across the full width, wrapped, with clickable links
/// (features.md §2.3.3). Not shown when the torrent has no comment.
struct GeneralCommentRow: View {
    let comment: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Design.Spacing.small) {
            Text("Comment:")
                .foregroundStyle(.secondary)
            Text(LinkedText.cachedAttributed(comment))
                .modifier(SelectableValue())
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
