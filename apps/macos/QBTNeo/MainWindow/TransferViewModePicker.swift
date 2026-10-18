import SwiftUI

/// The main toolbar's List / Cards switch, like Finder's view picker: a
/// two-segment control with symbols, a tooltip per segment with the View
/// menu's command and shortcut (⌘1 / ⌘2), and the mode's name for
/// VoiceOver. It edits the same setting as the View menu, the Command
/// Palette and Settings, so all of them stay in sync.
struct TransferViewModePicker: View {
    @Bindable var gui: GUIPreferences

    var body: some View {
        Picker(String(localized: "viewMode.picker", defaultValue: "View", comment: "Toolbar: label of the List / Cards switch"),
               selection: $gui.transferViewMode) {
            ForEach(TransferViewMode.allCases) { mode in
                Label(mode.shortTitle, systemImage: mode.symbol)
                    .help(Self.tooltip(mode))
                    .tag(mode)
            }
        }
        .pickerStyle(.segmented)
        .labelStyle(.iconOnly)
    }

    /// "View as List (⌘1)": the command's title and shortcut.
    static func tooltip(_ mode: TransferViewMode) -> String {
        let command = CommandRegistry[mode == .list ? .viewAsList : .viewAsCards]
        guard let shortcut = command.shortcut else { return command.title }
        return "\(command.title) (\(shortcut.display))"
    }
}
