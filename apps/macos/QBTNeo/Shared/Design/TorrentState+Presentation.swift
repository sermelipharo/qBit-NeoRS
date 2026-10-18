import AppKit
import SwiftUI

/// Status text, icon and color of each torrent state (features.md §1.7).
/// Colors are qBittorrent's GitHub Primer defaults (light / dark).
extension TorrentState {
    var statusText: String {
        switch self {
        case .unknown: ""
        case .forcedDownloading: String(localized: "[F] Downloading")
        case .downloading: String(localized: "Downloading")
        case .forcedDownloadingMetadata: String(localized: "[F] Downloading metadata")
        case .downloadingMetadata: String(localized: "Downloading metadata")
        case .stalledDownloading: String(localized: "Stalled")
        case .forcedUploading: String(localized: "[F] Seeding")
        case .uploading, .stalledUploading: String(localized: "Seeding")
        case .checkingResumeData: String(localized: "Checking resume data")
        case .queuedDownloading, .queuedUploading: String(localized: "Queued")
        case .checkingUploading, .checkingDownloading: String(localized: "Checking")
        case .stoppedDownloading: String(localized: "Stopped")
        case .stoppedUploading: String(localized: "Completed")
        case .moving: String(localized: "Moving")
        case .missingFiles: String(localized: "Missing Files")
        case .error: String(localized: "Errored")
        }
    }

    var systemImage: String {
        switch self {
        case .unknown, .missingFiles, .error: Symbol.exclamationmarkTriangleFill
        case .forcedDownloading, .downloading: Symbol.arrowDownCircleFill
        case .forcedDownloadingMetadata, .downloadingMetadata: Symbol.arrowDownCircleDotted
        case .stalledDownloading: Symbol.arrowDownCircle
        case .forcedUploading, .uploading: Symbol.arrowUpCircleFill
        case .stalledUploading: Symbol.arrowUpCircle
        case .checkingResumeData, .checkingUploading, .checkingDownloading: Symbol.arrowTrianglehead2ClockwiseRotate90CircleFill
        case .queuedDownloading, .queuedUploading: Symbol.clockFill
        case .stoppedDownloading: Symbol.pauseCircleFill
        case .stoppedUploading: Symbol.checkmarkCircleFill
        case .moving: Symbol.folderCircleFill
        }
    }

    var color: Color { Color(nsColor: nsColor) }

    /// The state color for AppKit views (same values as `color`).
    var nsColor: NSColor {
        switch self {
        case .unknown: .secondaryLabelColor
        case .forcedDownloading, .downloading, .forcedDownloadingMetadata, .downloadingMetadata,
             .checkingResumeData, .checkingUploading, .checkingDownloading, .moving:
            Self.green
        case .stalledDownloading: Self.stalledGreen
        case .forcedUploading, .uploading: Self.blue
        case .stalledUploading: Self.stalledBlue
        case .queuedDownloading, .queuedUploading: Self.brown
        case .stoppedDownloading: Self.gray
        case .stoppedUploading: Self.purple
        case .missingFiles, .error: Self.red
        }
    }

    /// The state color for small bold text on a card (the status word): the
    /// same hue, lightened or darkened where `nsColor` is below 4.5:1 on the
    /// card background (stalled green and blue, and brown, gray, purple and
    /// red in dark mode). Measured on #F9F9F9 / #2F2F2F: 4.8–6.8 : 1.
    var cardTextNSColor: NSColor {
        switch self {
        case .stalledDownloading: Self.green
        case .stalledUploading: Self.blue
        case .queuedDownloading, .queuedUploading: Self.brownText
        case .stoppedDownloading: Self.grayText
        case .stoppedUploading: Self.purpleText
        case .missingFiles, .error: Self.redText
        default: nsColor
        }
    }

    private static let green = NSColor(light: 0x1A7F37, dark: 0x3FB950)
    private static let stalledGreen = NSColor(light: 0x2DA44E, dark: 0x238636)
    private static let blue = NSColor(light: 0x0969DA, dark: 0x58A6FF)
    private static let stalledBlue = NSColor(light: 0x0969DA, dark: 0x1F6FEB)
    private static let brown = NSColor(light: 0x7D4E00, dark: 0xA0700F)
    private static let gray = NSColor(light: 0x57606A, dark: 0x8B949E)
    private static let purple = NSColor(light: 0x8250DF, dark: 0xA371F7)
    private static let red = NSColor(light: 0xCF222E, dark: 0xF85149)
    private static let brownText = NSColor(light: 0x7D4E00, dark: 0xD29922)
    private static let grayText = NSColor(light: 0x57606A, dark: 0x9198A1)
    private static let purpleText = NSColor(light: 0x8250DF, dark: 0xBC8CFF)
    private static let redText = NSColor(light: 0xCF222E, dark: 0xFF7B72)

    // MARK: Classification (TorrentFilter::matchStatus)

    var isDownloading: Bool {
        switch self {
        case .downloading, .downloadingMetadata, .forcedDownloadingMetadata, .stalledDownloading,
             .checkingDownloading, .stoppedDownloading, .queuedDownloading, .forcedDownloading:
            true
        default:
            false
        }
    }

    var isUploading: Bool {
        switch self {
        case .uploading, .stalledUploading, .checkingUploading, .queuedUploading, .forcedUploading: true
        default: false
        }
    }

    var isCompleted: Bool {
        isUploading || self == .stoppedUploading
    }

    var isStopped: Bool { self == .stoppedDownloading || self == .stoppedUploading }
    var isQueued: Bool { self == .queuedDownloading || self == .queuedUploading }
    var isChecking: Bool { self == .checkingDownloading || self == .checkingUploading || self == .checkingResumeData }
    var isErrored: Bool { self == .error || self == .missingFiles }
    var isForced: Bool {
        self == .forcedDownloading || self == .forcedUploading || self == .forcedDownloadingMetadata
    }

    /// The progress bar is drawn disabled for these states.
    var dimsProgressBar: Bool { self == .error || self == .stoppedDownloading || self == .unknown }
}
