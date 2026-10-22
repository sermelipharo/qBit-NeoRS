import Foundation

/// Tracker lists for the tracker editors: downloaded from a URL (by the core,
/// through the general-purpose proxy) or read from a text file.
extension AppModel {
    /// Lists larger than this are not tracker lists.
    nonisolated static let trackerListSizeLimit = 4 * 1024 * 1024

    func downloadTrackerList(_ url: String) async throws -> String {
        guard let client else { throw QbtError.Engine(message: "The session is not running.") }
        return try await client.downloadTrackerList(url: url)
    }

    /// The text of a tracker list file chosen in an open panel.
    nonisolated static func readTrackerListFile(_ url: URL) throws -> String {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let data = try handle.read(upToCount: trackerListSizeLimit + 1) ?? Data()
        guard data.count <= trackerListSizeLimit else {
            throw QbtError.InvalidArgument(message: "The file is larger than 4 MB.")
        }
        guard let text = String(data: data, encoding: .utf8) else {
            throw QbtError.InvalidArgument(message: "The file is not UTF-8 text.")
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// `text` with `list` appended on a new line, like qBittorrent's download button.
    nonisolated static func appendingTrackerList(_ list: String, to text: String) -> String {
        guard !list.isEmpty else { return text }
        if text.isEmpty { return list }
        return text + (text.hasSuffix("\n") ? "" : "\n") + list
    }
}
