import SwiftUI

/// The import sheets in snapshot mode (`--snapshot-only import`), with demo
/// data: nothing is read from or written to a real profile.
@MainActor
enum ImportSnapshots {
    static var entries: [FeatureSnapshotCatalog.Entry] {
        let size = ImportSheet.size
        return [
            sheet("import-transmission", ImportSheetRequest(sources: [.transmission]), client: DemoImportClient(), size: size),
            sheet("import-qbittorrent", ImportSheetRequest(sources: [.qBittorrent]), client: DemoImportClient(), size: size),
            sheet("import-offer", ImportSheetRequest(sources: ImportSource.all, isOffer: true), client: DemoImportClient(), size: size),
            sheet("import-running", ImportSheetRequest(sources: [.transmission]), client: DemoImportClient(running: [.transmission]), size: size),
            sheet("import-merge", ImportSheetRequest(sources: [.transmission]), client: DemoImportClient(profileHasTorrents: true), size: size),
            FeatureSnapshotCatalog.Entry(name: "import-result", title: AppWindowID.main.title,
                                         size: CGSize(width: Design.Window.wideDialogWidth, height: 200), active: true) { _ in
                AnyView(ImportResultSheet(outcome: sampleOutcome))
            },
        ]
    }

    private static func sheet(_ name: String, _ request: ImportSheetRequest, client: DemoImportClient, size: CGSize) -> FeatureSnapshotCatalog.Entry {
        FeatureSnapshotCatalog.Entry(name: name, title: AppWindowID.main.title, size: size, active: true) { _ in
            AnyView(ImportSheet(request: request, client: client, profileDirectory: ""))
        }
    }

    private static var sampleOutcome: ImportOutcome {
        ImportOutcome(
            plan: DemoImportClient.samplePlan(.transmission, merge: false, profileHasTorrents: false),
            backupDir: "/Users/demo/Library/Application Support/qBit NeoRS.before-migration-20261018-221500",
            reportPath: "/Users/demo/Library/Application Support/qBit NeoRS/migration/report.txt"
        )
    }
}
