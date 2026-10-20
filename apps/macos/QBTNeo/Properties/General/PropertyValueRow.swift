import SwiftUI

/// One "Label: value" pair inside a General tab grid row. Missing values are
/// drawn dimmed ("—" when empty) so the rows keep their place.
struct PropertyValueRow: View {
    let label: LocalizedStringKey
    let value: String
    var help: LocalizedStringKey?
    var isPlaceholder = false
    var action: PropertyValueAction?

    var body: some View {
        // Only labels with a help text get a tooltip (each one costs tracking).
        if let help {
            labelText.help(Text(help))
        } else {
            labelText
        }
        let dimmed = isPlaceholder || value.isEmpty
        HStack(spacing: Design.Spacing.tight) {
            Text(value.isEmpty ? "—" : value)
                .monospacedDigit()
                .foregroundStyle(dimmed ? .tertiary : .primary)
                .modifier(SelectableValue())
                .lineLimit(1)
                .truncationMode(.middle)
                .help(value)
            if let action, !dimmed {
                PropertyValueActionButton(action: action, value: value)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .gridColumnAlignment(.leading)
    }

    private var labelText: some View {
        Text(label)
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .fixedSize()
            .gridColumnAlignment(.trailing)
    }
}
