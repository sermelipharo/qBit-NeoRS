import Foundation

/// Bounding of strings written into the shared snapshot.
enum WidgetText {
    /// At most `limit` characters; longer text ends (or starts) with "…".
    static func bounded(_ text: String, _ limit: Int, keepingEnd: Bool = false) -> String {
        guard limit > 0 else { return "" }
        guard text.count > limit else { return text }
        return keepingEnd ? "…" + String(text.suffix(limit - 1)) : String(text.prefix(limit - 1)) + "…"
    }

    /// The last `components` folders of a path ("/Users/a/Downloads/Linux/" → "Downloads/Linux").
    static func pathTail(_ path: String, components: Int = 2) -> String {
        let parts = path.split(separator: "/", omittingEmptySubsequences: true)
        return parts.suffix(components).joined(separator: "/")
    }

    /// An info hash as the app uses it: 1–64 lowercase hexadecimal characters.
    static func isTorrentID(_ text: String) -> Bool {
        (1...64).contains(text.utf8.count) && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
}
