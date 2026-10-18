import SwiftUI

/// A parse result as the cards show it: the title (semibold) and the tags
/// under it; optionally the original name in small secondary text.
struct ParsedNamePreview: View {
    let name: String
    let parsed: ParsedName
    var showsName = true

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            Text(parsed.title)
                .fontWeight(.semibold)
                .lineLimit(2)
                .textSelection(.enabled)
            if parsed.chips.isEmpty {
                Text(String(localized: "names.preview.noTags", defaultValue: "No technical info found",
                            comment: "Settings preview: no technical info was found in a name"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ChipFlowLayout {
                    ForEach(Array(parsed.chips.enumerated()), id: \.offset) { _, chip in
                        NameChipLabel(chip)
                    }
                }
            }
            if showsName {
                Text(verbatim: name)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(name)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}
