import Foundation

/// `qbitneors://torrent/<hash>`: opens the app with that torrent selected.
enum WidgetLink {
    static let scheme = "qbitneors"

    static func torrent(_ id: String) -> URL? {
        guard WidgetText.isTorrentID(id) else { return nil }
        return URL(string: "\(scheme)://torrent/\(id)")
    }

    /// The torrent of a link, or nil for anything else.
    static func torrentID(from url: URL) -> String? {
        guard url.scheme?.lowercased() == scheme, url.host()?.lowercased() == "torrent" else { return nil }
        let parts = url.path(percentEncoded: true).split(separator: "/")
        guard parts.count == 1 else { return nil }
        let id = String(parts[0]).lowercased()
        return WidgetText.isTorrentID(id) ? id : nil
    }
}
