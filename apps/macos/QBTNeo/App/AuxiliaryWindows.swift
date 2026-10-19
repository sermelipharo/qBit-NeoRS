import SwiftUI

/// Windows owned by other features (Creator, Statistics, Cookies, About).
/// Search, RSS and the log are tabs of the main window.
struct AuxiliaryWindows: Scene {
    let model: AppModel

    var body: some Scene {
        Window(AppWindowID.creator.title, id: AppWindowID.creator.rawValue) {
            TorrentCreatorWindow().lockGated().environment(model)
        }
        .restorationBehavior(.disabled)
        .defaultSize(width: 620, height: 700)

        Window(AppWindowID.statistics.title, id: AppWindowID.statistics.rawValue) {
            StatisticsWindow().lockGated().environment(model).releasedWhileHidden()
        }
        .restorationBehavior(.disabled)
        .defaultSize(width: 420, height: 620)

        Window(AppWindowID.cookies.title, id: AppWindowID.cookies.rawValue) {
            CookiesWindow().lockGated().environment(model)
        }
        .restorationBehavior(.disabled)
        .defaultSize(width: 720, height: 420)

        Window(FeatureWindowID.rssDownloader.title, id: FeatureWindowID.rssDownloader.rawValue) {
            RSSDownloaderWindow().lockGated().environment(model).environment(model.gui)
        }
        .restorationBehavior(.disabled)
        .defaultSize(width: 1_180, height: 720)

        Window(FeatureWindowID.searchPlugins.title, id: FeatureWindowID.searchPlugins.rawValue) {
            SearchPluginsWindow().lockGated().environment(model)
        }
        .restorationBehavior(.disabled)
        .defaultSize(width: 720, height: 460)

        Window(AppWindowID.about.title, id: AppWindowID.about.rawValue) {
            AboutWindow().lockGated().environment(model).releasedWhileHidden()
        }
        .restorationBehavior(.disabled)
        .windowResizability(.contentSize)
    }
}
