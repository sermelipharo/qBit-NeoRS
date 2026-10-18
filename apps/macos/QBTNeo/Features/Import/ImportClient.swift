import Foundation

/// The core's import calls (`migrate/import.rs`), so the import sheet can run
/// against demo data in snapshot mode. They block on disk I/O: `ImportModel`
/// calls them off the main actor.
protocol ImportClient: Sendable {
    func detect(_ source: ImportSource) -> ImportDetection
    /// Dry run into the profile; nothing is written.
    func plan(_ source: ImportSource, profileDirectory: String, keepSettings: Bool) throws -> ImportPlan
    /// Records the import for the next launch.
    func request(_ source: ImportSource, profileDirectory: String, keepSettings: Bool) throws
    /// Whether the source application is open.
    func isRunning(_ source: ImportSource) -> Bool
}
