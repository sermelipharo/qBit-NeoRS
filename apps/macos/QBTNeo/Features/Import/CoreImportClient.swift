import AppKit

/// `ImportClient` backed by the Rust core and the running applications list.
struct CoreImportClient: ImportClient {
    func detect(_ source: ImportSource) -> ImportDetection {
        importDetect(source: source)
    }

    func plan(_ source: ImportSource, profileDirectory: String, keepSettings: Bool) throws -> ImportPlan {
        try importPlan(source: source, profileDir: profileDirectory, keepSettings: keepSettings)
    }

    func request(_ source: ImportSource, profileDirectory: String, keepSettings: Bool) throws {
        try importRequest(source: source, profileDir: profileDirectory, keepSettings: keepSettings)
    }

    func isRunning(_ source: ImportSource) -> Bool {
        !NSRunningApplication.runningApplications(withBundleIdentifier: source.bundleIdentifier).isEmpty
    }
}
