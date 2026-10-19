/// The system color of each tag category, shared by the cards
/// (`NameChipStyle`) and the Torrent widget.
extension NameRuleCategory {
    var chipTint: WidgetNameChip.Tint {
        switch self {
        case .seasonEpisode, .volume: .red
        case .resolution: .blue
        case .hdr: .orange
        case .source: .purple
        case .videoCodec: .teal
        case .audio: .indigo
        case .audioLanguage: .green
        case .subtitles: .yellow
        case .edition: .brown
        case .platform, .version: .cyan
        case .region: .green
        case .format: .pink
        case .year, .releaseGroup, .hidden: .gray
        case .other: .mint
        }
    }
}
