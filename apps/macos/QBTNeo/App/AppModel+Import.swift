import Foundation

/// Importing from qBittorrent or Transmission (`Features/Import`). The
/// import itself runs at the next launch, before the core starts: the
/// running core is never stopped and restarted in place (see
/// `migrate/import.rs`).
extension AppModel {
    /// GUI defaults key: the first-launch offer was shown.
    static let importOfferShownKey = "qBitNeoRS/ImportOfferShown"

    /// Demo and snapshot runs never touch a real profile.
    var canImport: Bool { !launch.usesDemoData }

    var importClient: any ImportClient {
        launch.usesDemoData ? DemoImportClient() : CoreImportClient()
    }

    func showImport(_ source: ImportSource) {
        guard canImport else { return }
        sheet = .importData(ImportSheetRequest(sources: [source]))
    }

    /// Quits normally (resume data saved, core stopped) and opens again; the
    /// new instance runs the recorded import before creating its core.
    func restartForImport() {
        isRestartingForImport = true
        termination.requestQuit()
    }

    /// On the first launch with an empty profile, offers the import when
    /// qBittorrent or Transmission data is found (once).
    func offerImportIfNeeded() {
        guard canImport, !gui.defaults.bool(forKey: Self.importOfferShownKey) else { return }
        let profile = launch.profileDirectory
        Task { [weak self] in
            let found = await Self.detectSources(profile: profile)
            guard let self, !found.isEmpty, sheet == nil, !lock.isLocked else { return }
            gui.defaults.set(true, forKey: Self.importOfferShownKey)
            sheet = .importData(ImportSheetRequest(sources: found, isOffer: true))
        }
    }

    @concurrent
    private static func detectSources(profile: String) async -> [ImportSource] {
        guard importProfileIsEmpty(profileDir: profile) else { return [] }
        return ImportSource.all.filter { source in
            let detection = importDetect(source: source)
            return detection.found && detection.torrents > 0
        }
    }

    /// Runs an import recorded before the restart. Called by `start()` before
    /// the core exists.
    func runPendingImport() async {
        guard canImport, let request = importPending(profileDir: launch.profileDirectory) else { return }
        let source = request.source
        if CoreImportClient().isRunning(source) {
            importCancelPending(profileDir: launch.profileDirectory)
            let app = source.appName
            let message = String(localized: "\(app) was open, so nothing was imported. Quit it and choose File > Import from \(app)… again.")
            alert = AppAlert(title: String(localized: "Import Cancelled"), message: message)
            return
        }
        startupImport = source
        let result = await Self.runPending(profile: launch.profileDirectory)
        startupImport = nil
        switch result {
        case .success(let outcome?):
            sheet = .importResult(outcome)
        case .success(nil):
            break
        case .failure(let error):
            alert = AppAlert(title: String(localized: "Can't Import from \(source.appName)"), message: error.userMessage)
        }
    }

    @concurrent
    private static func runPending(profile: String) async -> Result<ImportOutcome?, any Error> {
        Result { try importRunPending(profileDir: profile) }
    }
}
