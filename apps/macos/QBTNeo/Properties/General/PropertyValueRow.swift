import SwiftUI

/// One "Label: value" pair inside a General tab grid row.
struct PropertyValueRow: View {
    let label: LocalizedStringKey
    let value: String
    var help: LocalizedStringKey?
    /// Grid columns the value spans (1 = its own column).
    var valueColumns = 1

    var body: some View {
        // Only labels with a help text get a tooltip (each one costs tracking).
        if let help {
            labelText.help(Text(help))
        } else {
            labelText
        }
        Text(value)
            .monospacedDigit()
            .modifier(SelectableValue())
            .lineLimit(1)
            .truncationMode(.middle)
            .frame(maxWidth: .infinity, alignment: .leading)
            .gridColumnAlignment(.leading)
            .gridCellColumns(valueColumns)
            .help(value)
    }

    private var labelText: some View {
        Text(label)
            .foregroundStyle(.secondary)
            .gridColumnAlignment(.trailing)
    }
}
