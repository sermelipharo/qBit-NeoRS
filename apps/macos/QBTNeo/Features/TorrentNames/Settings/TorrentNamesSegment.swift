import SwiftUI

/// Settings › Interface › Torrent Names: the master switch, per-view
/// options, the rules (edited in a sheet) and a preview of a few current
/// torrent names with the saved rules (qBit NeoRS extension).
struct TorrentNamesSegment: View {
    @Environment(AppModel.self) private var app
    @Environment(GUIPreferences.self) private var gui
    @State private var isEditingRules = false
    @State private var previews: [Preview] = []

    struct Preview: Identifiable {
        let name: String
        let parsed: ParsedName
        var id: String { name }
    }

    /// Shown when there are no torrents yet.
    private static let sampleNames = [
        "Movie.Name.2021.2160p.UHD.BluRay.REMUX.HDR.DV.TrueHD.7.1.Atmos.HEVC-GROUP",
        "[SubsPlease] Some Anime - 05 (1080p) [A1B2C3D4].mkv",
        "Show.Name.S01E02.1080p.AMZN.WEB-DL.DDP5.1.H.264.MultiSub-NTb",
        "Фильм / Film (2021) WEB-DLRip 1080p | Дубляж",
    ]

    var body: some View {
        @Bindable var gui = gui
        SettingsGroup(.torrentNames) {
            Toggle(SettingsCatalog.title("GUI/TorrentNames/Enabled"), isOn: $gui.parseTorrentNames)
                .setting("GUI/TorrentNames/Enabled")
        } content: {
            Group {
                Toggle(SettingsCatalog.title("GUI/TorrentNames/SemiboldTitles"), isOn: $gui.torrentNamesSemiboldTitles)
                    .setting("GUI/TorrentNames/SemiboldTitles", indented: true)
                Toggle(SettingsCatalog.title("GUI/TorrentNames/CardTags"), isOn: $gui.torrentNamesCardTags)
                    .setting("GUI/TorrentNames/CardTags", indented: true)
                Toggle(SettingsCatalog.title("GUI/TorrentNames/YearAsTag"), isOn: $gui.torrentNamesYearAsTag)
                    .setting("GUI/TorrentNames/YearAsTag", indented: true)
            }
            .disabled(!gui.parseTorrentNames)
        }

        SettingsGroup(.nameRules) {
            HStack(spacing: Design.Spacing.regular) {
                Button(String(localized: "settings.names.editRules", defaultValue: "Edit Rules…", comment: "Settings: opens the name rules editor")) {
                    isEditingRules = true
                }
                Text(rulesSummary)
                    .foregroundStyle(.secondary)
            }
            .setting("GUI/TorrentNames/Rules")
        } content: {
            LabeledContent(String(localized: "settings.names.preview", defaultValue: "Preview:", comment: "Settings: name parsing preview")) {
                VStack(alignment: .leading, spacing: Design.Spacing.medium) {
                    ForEach(previews) { preview in
                        ParsedNamePreview(name: preview.name, parsed: preview.parsed)
                    }
                }
                .frame(maxWidth: SettingsLayout.footnoteWidth, alignment: .leading)
            }
        }
        .sheet(isPresented: $isEditingRules) {
            NameRulesEditor(names: app.names)
        }
        // Parsed here, not in `body`: when shown and whenever the rules or options change.
        .task(id: app.names.revision) { updatePreviews() }
    }

    private var rulesSummary: String {
        let names = app.names
        let enabled = names.rules.count { $0.isEnabled }
        return names.isCustomized
            ? String(localized: "settings.names.summary.custom", defaultValue: "\(enabled) of \(names.rules.count) rules on (edited)",
                     comment: "Settings: name rules summary")
            : String(localized: "settings.names.summary.builtIn", defaultValue: "\(enabled) built-in rules",
                     comment: "Settings: name rules summary")
    }

    private func updatePreviews() {
        let current = app.list.visibleItems.prefix(4).map(\.name)
        let names = current.isEmpty ? Self.sampleNames : Array(current)
        previews = names.map { Preview(name: $0, parsed: app.names.parsedIgnoringSwitch($0)) }
    }
}
