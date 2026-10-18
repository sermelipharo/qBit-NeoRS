import SwiftUI

/// The dry run of an import: counts, the settings choice, what doesn't carry
/// over, warnings and the full report.
struct ImportPlanForm: View {
    @Bindable var importModel: ImportModel
    let plan: ImportPlan

    var body: some View {
        let app = plan.source.appName
        Form {
            Section("Will Be Imported") {
                count("Torrents", plan.torrents)
                if plan.torrentsWithPieces > 0 {
                    count("Progress kept without a recheck", plan.torrentsWithPieces)
                }
                if plan.torrentsToCheck > 0 {
                    count("Checked when they start", plan.torrentsToCheck)
                }
                if plan.magnets > 0 {
                    count("Magnet links waiting for metadata", plan.magnets)
                }
                if plan.torrentsMerged > 0 {
                    count("Already in qBit NeoRS (statistics added)", plan.torrentsMerged)
                }
                count("Categories", plan.categories)
                count("Tags", plan.tags)
                count("Watched folders", plan.watchedFolders)
                if plan.source == .qBittorrent {
                    count("RSS feeds", plan.rssFeeds)
                    count("Search plugins", plan.searchPlugins)
                }
                LabeledContent("Settings") {
                    Text(plan.merge ? "Current settings kept" : "Taken from \(app)")
                }
            }
            Section {
                // A profile with torrents always keeps its settings (merge).
                Toggle("Keep my current qBit NeoRS settings", isOn: Binding(
                    get: { importModel.keepSettings || plan.profileHasTorrents },
                    set: { importModel.keepSettings = $0 }
                ))
                    .disabled(plan.profileHasTorrents)
            } footer: {
                Text(settingsFooter(app))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Section("Not Carried Over") {
                ForEach(notCarried(app), id: \.self) { line in
                    Label(line, systemImage: Symbol.xmarkCircle)
                        .labelStyle(.titleAndIcon)
                }
            }
            if !plan.warnings.isEmpty || plan.torrentsFailed > 0 {
                Section("Warnings") {
                    if plan.torrentsFailed > 0 {
                        Label("\(plan.torrentsFailed) torrent(s) can't be imported (see the report)", systemImage: Symbol.exclamationmarkTriangleFill)
                    }
                    ForEach(plan.warnings, id: \.self) { Text($0).font(.callout) }
                }
            }
            Section {
                DisclosureGroup("Full Report") {
                    ScrollView {
                        Text(plan.report)
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(height: 180)
                }
            }
        }
        .formStyle(.grouped)
    }

    private func count(_ title: LocalizedStringKey, _ value: UInt32) -> some View {
        LabeledContent(title) {
            Text(Int(value), format: .number)
                .monospacedDigit()
        }
    }

    private func settingsFooter(_ app: String) -> String {
        if plan.profileHasTorrents {
            return String(localized: "qBit NeoRS already has torrents: its settings are kept, its torrents keep their state, and the others are added.")
        }
        return plan.merge
            ? String(localized: "Settings stay as they are; torrents, categories and tags are added.")
            : String(localized: "qBit NeoRS takes over \(app)'s settings: folders, speed limits, connection, queue and seeding limits.")
    }

    /// What never carries over from the source (the report has the details).
    private func notCarried(_ app: String) -> [String] {
        switch plan.source {
        case .transmission:
            var lines = [
                String(localized: "Group colors and automatic group rules"),
                String(localized: "Known peers, bandwidth priority and per-torrent peer limits"),
                String(localized: "The blocklist and the DHT cache (built again)"),
                String(localized: "Window layout and other settings only Transmission has"),
            ]
            if plan.remoteAccessEnabled {
                let rpc = String(localized: "Remote access (RPC) is on in Transmission. Its password is not copied; qBit NeoRS offers the Web UI instead.")
                lines.insert(rpc, at: 0)
            }
            return lines
        case .qBittorrent:
            return [
                String(localized: "Window layout, column widths and Qt styles"),
                String(localized: "Cookies (add them again in Manage Cookies)"),
                String(localized: "Partially downloaded pieces and known peers"),
                String(localized: "Logs and the GeoIP database (downloaded again)"),
            ]
        }
    }
}
