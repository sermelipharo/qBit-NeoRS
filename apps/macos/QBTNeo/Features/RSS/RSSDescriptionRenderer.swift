import AppKit
import SwiftUI

/// Turns an article description into display text without loading anything
/// remote and without scripts (features.md §5.12.1 "renderArticle"): rich
/// HTML is sanitized (scripts, styles, frames, embeds and images removed,
/// relative links made absolute) and imported as attributed text; plain
/// text has its BBCode converted first. Colors and fonts follow the system.
@MainActor
enum RSSDescriptionRenderer {
    /// Longest description rendered (characters); the rest is cut. Feeds are
    /// untrusted and rendering runs on the main actor, so its cost must stay
    /// bounded (the core allows up to 1 MiB per description).
    static let maxRenderedLength = 64 * 1024

    static func render(_ description: String, link: String) -> AttributedString {
        let capped = description.count > maxRenderedLength ? String(description.prefix(maxRenderedLength)) + "…" : description
        let trimmed = capped.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return AttributedString() }
        let html = mightBeRichText(trimmed) ? trimmed : "<pre>\(bbCodeToHTML(escape(trimmed)))</pre>"
        let sanitized = absolutizeLinks(sanitize(html), base: link)
        let wrapped = "<html><head><meta charset=\"utf-8\"></head><body>\(sanitized)</body></html>"
        guard let data = wrapped.data(using: .utf8),
              let imported = try? NSMutableAttributedString(
                data: data,
                options: [.documentType: NSAttributedString.DocumentType.html, .characterEncoding: String.Encoding.utf8.rawValue],
                documentAttributes: nil
              )
        else { return AttributedString(stripTags(trimmed)) }
        normalize(imported)
        let text = (try? AttributedString(imported, including: \.appKit)) ?? AttributedString(imported.string)
        return text
    }

    /// `Qt::mightBeRichText`: a tag near the start (only the start is
    /// searched; the regex is slow on long runs of unterminated tags).
    static func mightBeRichText(_ text: String) -> Bool {
        String(text.prefix(1024)).range(of: #"<\s*/?\s*[a-zA-Z][a-zA-Z0-9]*[^>]*>"#, options: .regularExpression) != nil
    }

    private static func escape(_ text: String) -> String {
        text.replacing("&", with: "&amp;").replacing("<", with: "&lt;").replacing(">", with: "&gt;")
    }

    /// qBittorrent's BBCode subset (`[b] [i] [u] [s] [url=] [color=] [size=]`); `[img]` becomes a link.
    static func bbCodeToHTML(_ text: String) -> String {
        var result = text
        let rules: [(String, String)] = [
            // Bounded repetitions keep these linear on hostile input.
            (#"\[img\](.{1,2048}?)\[/img\]"#, "<a href=\"$1\">$1</a>"),
            (#"\[url=(\"?)(.{1,2048}?)\1\]"#, "<a href=\"$2\">"),
            (#"\[/url\]"#, "</a>"),
            (#"\[(/)?([bius])\]"#, "<$1$2>"),
            (#"\[color=(\"?)(.{1,256}?)\1\]"#, "<span style=\"color:$2\">"),
            (#"\[/color\]"#, "</span>"),
            (#"\[size=(\"?)(.{0,256}?)\d\1\]"#, "<span>"),
            (#"\[/size\]"#, "</span>"),
        ]
        for (pattern, template) in rules {
            result = result.replacingOccurrences(of: pattern, with: template, options: [.regularExpression, .caseInsensitive])
        }
        return result
    }

    /// Removes everything that could load remote content or run code
    /// (allowlist: the output is rebuilt from formatting tags only).
    static func sanitize(_ html: String) -> String {
        RSSHTMLSanitizer.sanitize(html)
    }

    /// Relative `href`s resolved against the article link's `scheme://host/`.
    static func absolutizeLinks(_ html: String, base link: String) -> String {
        guard let url = URL(string: link), let scheme = url.scheme, let host = url.host() else { return html }
        let port = url.port.map { ":\($0)" } ?? ""
        let base = "\(scheme)://\(host)\(port)/"
        return html.replacingOccurrences(
            of: #"(<a\s+[^>]*?href\s*=\s*["'])(?!https?:|ftp:|mailto:|#)/?([^"']*)(["'])"#, with: "$1\(base)$2$3",
            options: [.regularExpression, .caseInsensitive]
        )
    }

    /// Removes `<...>` runs in one linear pass (a regex is quadratic on
    /// unterminated tags).
    static func stripTags(_ html: String) -> String {
        var out = String.UnicodeScalarView()
        var pending = String.UnicodeScalarView()
        var inTag = false
        for char in html.unicodeScalars {
            if inTag {
                pending.append(char)
                if char == ">" {
                    inTag = false
                    pending.removeAll(keepingCapacity: true)
                }
            } else if char == "<" {
                inTag = true
                pending.append(char)
            } else {
                out.append(char)
            }
        }
        // An unterminated `<...` stays text.
        out.append(contentsOf: pending)
        return String(out)
    }

    /// System fonts (keeping bold/italic/monospace traits) and no imported colors.
    private static func normalize(_ text: NSMutableAttributedString) {
        let range = NSRange(location: 0, length: text.length)
        text.removeAttribute(.foregroundColor, range: range)
        text.removeAttribute(.backgroundColor, range: range)
        let base = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        text.enumerateAttribute(.font, in: range) { value, subrange, _ in
            let traits = (value as? NSFont)?.fontDescriptor.symbolicTraits ?? []
            var font = traits.contains(.monoSpace) ? NSFont.monospacedSystemFont(ofSize: base.pointSize, weight: .regular) : base
            if let size = (value as? NSFont)?.pointSize, size > 16 { font = NSFont.systemFont(ofSize: min(size, 22), weight: .semibold) }
            let descriptor = font.fontDescriptor.withSymbolicTraits(traits.intersection([.bold, .italic]))
            text.addAttribute(.font, value: NSFont(descriptor: descriptor, size: font.pointSize) ?? font, range: subrange)
        }
        while text.string.hasSuffix("\n") { text.deleteCharacters(in: NSRange(location: text.length - 1, length: 1)) }
    }
}
