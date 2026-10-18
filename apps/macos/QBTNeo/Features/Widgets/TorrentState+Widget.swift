import Foundation

extension TorrentState {
    /// The coarser state the widgets show.
    var widgetState: WidgetTorrentState {
        switch self {
        case .forcedDownloading, .downloading: .downloading
        case .forcedDownloadingMetadata, .downloadingMetadata: .metadata
        case .stalledDownloading: .stalledDownloading
        case .forcedUploading, .uploading: .seeding
        case .stalledUploading: .stalledSeeding
        case .queuedDownloading, .queuedUploading: .queued
        case .checkingResumeData, .checkingUploading, .checkingDownloading: .checking
        case .moving: .moving
        case .stoppedDownloading, .unknown: .stopped
        case .stoppedUploading: .completed
        case .missingFiles: .missingFiles
        case .error: .error
        }
    }
}
