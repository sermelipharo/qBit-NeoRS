import AppKit
import SwiftUI

/// Shown after the restart of an import: what was imported, where the
/// backup and the report are.
struct ImportResultSheet: View {
    let outcome: ImportOutcome
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let plan = outcome.plan
        let app = plan.source.appName
        VStack(alignment: .leading, spacing: Design.Spacing.medium) {
            HStack(alignment: .top, spacing: Design.Spacing.medium) {
                Image(systemName: Symbol.checkmarkCircleFill)
                    .font(.largeTitle)
                    .foregroundStyle(.green)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: Design.Spacing.small) {
                    Text("Imported from \(app)")
                        .font(.headline)
                    let (all, kept, checked) = (Int(plan.torrents), Int(plan.torrentsWithPieces), Int(plan.torrentsToCheck))
                    Text("\(all) torrent(s) imported: \(kept) kept their progress, \(checked) are checked when they start.")
                        .fixedSize(horizontal: false, vertical: true)
                    if plan.torrentsFailed > 0 {
                        Text("\(Int(plan.torrentsFailed)) torrent(s) couldn't be imported; the report says why.")
                            .foregroundStyle(.secondary)
                    }
                    if !outcome.backupDir.isEmpty {
                        Text("The previous profile was backed up to \(outcome.backupDir).")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            HStack {
                if !outcome.backupDir.isEmpty {
                    Button("Show Backup in Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([URL(filePath: outcome.backupDir)])
                    }
                }
                Button("Open Report") {
                    NSWorkspace.shared.open(URL(filePath: outcome.reportPath))
                }
                Spacer()
                Button("OK") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(Design.Spacing.xLarge)
        .frame(width: Design.Window.wideDialogWidth)
    }
}
