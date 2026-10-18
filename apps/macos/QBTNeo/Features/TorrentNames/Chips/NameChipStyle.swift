import AppKit

/// The fixed look of each tag category: a system tint (adapts to dark mode and
/// Increase Contrast), filled or outlined, and a symbol shown when
/// "Differentiate without color" is on. Fill, text and border colors are
/// dynamic, derived from the tint per appearance:
/// * fill: the tint at 16 % (dark 26 %; with Increase Contrast 24 / 34 %);
/// * text: the tint mixed with 50 % black (dark: 35 % white; Increase
///   Contrast 62 / 55 %), so yellow and mint stay readable on light cards;
/// * border: only for outlined categories, or always with Increase Contrast.
@MainActor
enum NameChipStyle {
    struct Colors {
        let fill: NSColor
        let text: NSColor
        let border: NSColor?
    }

    nonisolated static let cornerRadius: CGFloat = 5
    nonisolated static let height: CGFloat = 18
    nonisolated static let horizontalPadding: CGFloat = 6
    nonisolated static let spacing: CGFloat = 4
    nonisolated static let lineSpacing: CGFloat = 4
    /// SF Pro Rounded, 11 pt medium: rounded digits and letters suit the rounded tags.
    static let font: NSFont = {
        let base = NSFont.systemFont(ofSize: 11, weight: .medium)
        return base.fontDescriptor.withDesign(.rounded).flatMap { NSFont(descriptor: $0, size: 11) } ?? base
    }()
    nonisolated static let symbolSize: CGFloat = 10
    nonisolated static let symbolGap: CGFloat = 3

    static func tint(_ category: NameRuleCategory) -> NSColor {
        switch category {
        case .seasonEpisode, .volume: .systemRed
        case .resolution: .systemBlue
        case .hdr: .systemOrange
        case .source: .systemPurple
        case .videoCodec: .systemTeal
        case .audio: .systemIndigo
        case .audioLanguage: .systemGreen
        case .subtitles: .systemYellow
        case .edition: .systemBrown
        case .platform, .version: .systemCyan
        case .region: .systemGreen
        case .format: .systemPink
        case .year, .releaseGroup, .hidden: .systemGray
        case .other: .systemMint
        }
    }

    /// Outlined categories have no fill: release groups and other flags are
    /// names rather than properties of the content. Volumes, versions and
    /// regions are outlined in the tint of their filled sibling (episodes,
    /// platforms, languages), so related tags read as related.
    static func isOutlined(_ category: NameRuleCategory) -> Bool {
        switch category {
        case .releaseGroup, .other, .volume, .version, .region: true
        default: false
        }
    }

    static func symbol(_ category: NameRuleCategory) -> String {
        switch category {
        case .seasonEpisode: Symbol.tv
        case .volume: Symbol.booksVertical
        case .resolution: Symbol.aspectratio
        case .hdr: Symbol.sunMax
        case .source: Symbol.opticaldisc
        case .videoCodec: Symbol.film
        case .audio: Symbol.speakerWave2
        case .audioLanguage: Symbol.characterBubble
        case .subtitles: Symbol.captionsBubble
        case .edition: Symbol.star
        case .platform: Symbol.desktopcomputer
        case .version: Symbol.number
        case .region: Symbol.globe
        case .format: Symbol.doc
        case .year: Symbol.calendar
        case .releaseGroup: Symbol.person2
        case .other: Symbol.tag
        case .hidden: Symbol.eyeSlash
        }
    }

    /// The neutral "+N" chip for tags that did not fit.
    static let overflow = Colors(
        fill: .quaternarySystemFill, text: .secondaryLabelColor, border: nil
    )

    /// The torrent's own category and tags (their icon keeps its color).
    static let label = Colors(
        fill: .quaternarySystemFill, text: .secondaryLabelColor, border: nil
    )

    private static var cache: [NameRuleCategory: Colors] = [:]

    static func colors(_ category: NameRuleCategory) -> Colors {
        if let cached = cache[category] { return cached }
        let tint = tint(category)
        let outlined = isOutlined(category)
        let made = Colors(
            fill: outlined ? .clear : dynamic(tint, light: 0.16, dark: 0.26, contrastLight: 0.24, contrastDark: 0.34, alphaOnly: true),
            text: dynamic(tint, light: 0.5, dark: 0.35, contrastLight: 0.62, contrastDark: 0.55, alphaOnly: false),
            border: outlined
                ? dynamic(tint, light: 0.55, dark: 0.6, contrastLight: 0.9, contrastDark: 0.9, alphaOnly: true)
                : dynamic(tint, light: 0, dark: 0, contrastLight: 0.7, contrastDark: 0.7, alphaOnly: true)
        )
        cache[category] = made
        return made
    }

    /// Whether the system asks for shapes instead of colors alone.
    static var showsSymbols: Bool {
        NSWorkspace.shared.accessibilityDisplayShouldDifferentiateWithoutColor
    }

    /// A color resolved per appearance: the tint at an alpha (`alphaOnly`),
    /// or the tint mixed toward black (light) or white (dark) by a fraction.
    private static func dynamic(
        _ tint: NSColor, light: CGFloat, dark: CGFloat, contrastLight: CGFloat, contrastDark: CGFloat, alphaOnly: Bool
    ) -> NSColor {
        NSColor(name: nil) { appearance in
            let match = appearance.bestMatch(from: [.aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua])
            let isDark = match == .darkAqua || match == .accessibilityHighContrastDarkAqua
            let isContrast = match == .accessibilityHighContrastAqua || match == .accessibilityHighContrastDarkAqua
            let amount = isContrast ? (isDark ? contrastDark : contrastLight) : (isDark ? dark : light)
            var resolved = tint
            appearance.performAsCurrentDrawingAppearance {
                resolved = tint.usingColorSpace(.sRGB) ?? tint
            }
            if alphaOnly { return resolved.withAlphaComponent(amount) }
            return resolved.blended(withFraction: amount, of: isDark ? .white : .black) ?? resolved
        }
    }
}
