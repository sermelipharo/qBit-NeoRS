import Foundation
import Observation

/// The import sheet's state: finds the source application's data, checks
/// that the application is quit, runs a dry run and, once confirmed, records
/// the import for the next launch (`AppModel.restartForImport`).
@Observable @MainActor
final class ImportModel {
    enum Phase: Sendable {
        case loading
        /// No data of the source application on this Mac.
        case notFound
        /// The source application is open: it saves its state when it quits.
        case sourceRunning
        case ready(ImportPlan)
        case failed(String)
    }

    /// The sources the user can choose between (one, unless the first-launch
    /// offer found both).
    let sources: [ImportSource]
    /// First-launch offer ("Not Now" instead of "Cancel", introduction text).
    let isOffer: Bool
    var source: ImportSource {
        didSet { if source != oldValue { reload() } }
    }
    /// Keep the profile's settings even though it has no torrents yet.
    var keepSettings = false {
        didSet { if keepSettings != oldValue { reload() } }
    }
    private(set) var phase: Phase = .loading
    private(set) var detection: ImportDetection?

    @ObservationIgnored private let client: any ImportClient
    @ObservationIgnored private let profileDirectory: String
    @ObservationIgnored private var task: Task<Void, Never>?

    init(request: ImportSheetRequest, client: any ImportClient, profileDirectory: String) {
        sources = request.sources
        isOffer = request.isOffer
        source = request.sources.first ?? .transmission
        self.client = client
        self.profileDirectory = profileDirectory
    }

    var plan: ImportPlan? {
        if case .ready(let plan) = phase { return plan }
        return nil
    }

    /// Something to import, and the source application is quit.
    var canImport: Bool {
        guard let plan else { return false }
        return plan.torrents > 0 || plan.categories > 0 || !plan.merge
    }

    /// Detection, the running check and the dry run, off the main actor.
    func reload() {
        task?.cancel()
        phase = .loading
        let (client, source, profile, keep) = (client, source, profileDirectory, keepSettings)
        task = Task { [weak self] in
            let result = await Self.load(client: client, source: source, profile: profile, keepSettings: keep)
            guard !Task.isCancelled, let self else { return }
            detection = result.detection
            phase = result.phase
        }
    }

    @concurrent
    private static func load(client: any ImportClient, source: ImportSource, profile: String, keepSettings: Bool)
        async -> (detection: ImportDetection, phase: Phase) {
        let detection = client.detect(source)
        guard detection.found else { return (detection, .notFound) }
        if client.isRunning(source) { return (detection, .sourceRunning) }
        do {
            return (detection, .ready(try client.plan(source, profileDirectory: profile, keepSettings: keepSettings)))
        } catch {
            return (detection, .failed(error.userMessage))
        }
    }

    /// Records the import; false (and the running state) when the source
    /// application was opened again meanwhile.
    func confirm() -> Bool {
        guard plan != nil else { return false }
        if client.isRunning(source) {
            phase = .sourceRunning
            return false
        }
        do {
            try client.request(source, profileDirectory: profileDirectory, keepSettings: keepSettings)
            return true
        } catch {
            phase = .failed(error.userMessage)
            return false
        }
    }

    func cancel() {
        task?.cancel()
    }
}
