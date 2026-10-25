import Foundation

/// The files of one format in a torrent: how many, and their total size.
/// `format` is the uppercased extension, empty for files without one.
struct FileFormatShare: Equatable, Sendable {
    let format: String
    private(set) var count = 0
    private(set) var size: Int64 = 0

    init(format: String) {
        self.format = format
    }

    mutating func add(_ bytes: Int64) {
        count += 1
        size += bytes
    }

    /// "MKV 3.81 GB", "SRT ×3 262 KB", "Other 4 KB".
    var description: String {
        let name = format.isEmpty ? String(localized: "Other") : format
        let size = UnitFormatter.size(size)
        return count > 1 ? "\(name) ×\(count) \(size)" : "\(name) \(size)"
    }
}
