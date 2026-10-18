import SwiftUI

/// Settings › Interface, in two segments: Main Window (list, sidebar,
/// details, status bar) and Torrent Names (titles and tags from names).
struct InterfacePane: View {
    @Bindable var model: PreferencesModel
    @Environment(SettingsNavigator.self) private var navigator

    var body: some View {
        @Bindable var navigator = navigator
        SettingsPaneView(.interface, model: model) {
            Picker("Interface", selection: $navigator.interfaceSegment) {
                ForEach(InterfaceSegment.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
            .padding(.top, SettingsLayout.sectionSpacing)
            .padding(.bottom, Design.Spacing.small)
        } content: {
            switch navigator.interfaceSegment {
            case .mainWindow: InterfaceMainWindowSegment(model: model)
            case .torrentNames: TorrentNamesSegment()
            }
        }
    }
}
