import Foundation

/// Turns URLs inside plain text into links (qBittorrent's `parseHtmlLinks`),
/// for torrent comments. Comments are untrusted (they come from .torrent
/// files), so only web links become clickable: `file:`, `smb:` or custom
/// app schemes would hand attacker-chosen targets to LaunchServices.
enum LinkedText {
    static let linkSchemes: Set<String> = ["http", "https", "ftp"]

    /// The last text and its result. The General tab evaluates its body on
    /// every refresh tick, while a torrent's comment practically never changes.
    @MainActor private static var last: (text: String, result: AttributedString)?

    /// `attributed(_:)`, remembering the last result.
    @MainActor
    static func cachedAttributed(_ text: String) -> AttributedString {
        if let last, last.text == text { return last.result }
        let result = attributed(text)
        last = (text, result)
        return result
    }

    static func attributed(_ text: String) -> AttributedString {
        var result = AttributedString(text)
        guard !text.isEmpty, let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return result }
        let range = NSRange(text.startIndex..., in: text)
        for match in detector.matches(in: text, range: range) {
            guard let url = match.url,
                  let scheme = url.scheme?.lowercased(), linkSchemes.contains(scheme),
                  let textRange = Range(match.range, in: text),
                  let lower = AttributedString.Index(textRange.lowerBound, within: result),
                  let upper = AttributedString.Index(textRange.upperBound, within: result)
            else { continue }
            result[lower..<upper].link = url
        }
        return result
    }
}
