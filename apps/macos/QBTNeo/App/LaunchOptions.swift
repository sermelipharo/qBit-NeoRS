import Foundation

/// Command-line switches:
/// * `--demo`: use the in-process sample session instead of the Rust core.
/// * `--demo-large`: demo with 2,000 torrents (performance testing).
/// * `--snapshot <dir>`: render key windows to PNG files with demo data, then exit.
/// * `--profile <dir>`: use another profile folder for the real core.
/// * `--assume-visible`: refresh as if the main window were always visible
///   (idle measurements of a window started in the background).
/// * `--widgets`: feed the desktop widgets from this instance, even with no
///   widget installed (testing). Otherwise only the default profile feeds
///   them, and only while widgets exist, because every instance shares the
///   widgets' App Group container.
/// * `--widget-pin <hash>`: publish that torrent as if a Torrent widget
///   followed it (testing; repeatable).
struct LaunchOptions: Sendable {
    var demo = false
    var demoLarge = false
    var snapshotDirectory: URL?
    var profileDirectory = ""
    var assumesVisibleWindow = false
    var widgetsRequested = false
    var widgetPins: [String] = []

    static let current = Self(arguments: CommandLine.arguments)

    init(arguments: [String]) {
        var iterator = arguments.dropFirst().makeIterator()
        while let argument = iterator.next() {
            switch argument {
            case "--demo": demo = true
            case "--demo-large": demoLarge = true
            case "--snapshot":
                if let path = iterator.next() { snapshotDirectory = URL(filePath: path, directoryHint: .isDirectory) }
            case "--profile":
                profileDirectory = iterator.next() ?? ""
            case "--assume-visible": assumesVisibleWindow = true
            case "--widgets": widgetsRequested = true
            case "--widget-pin":
                if let id = iterator.next() { widgetPins.append(id.lowercased()) }
            default: break
            }
        }
    }

    var isSnapshot: Bool { snapshotDirectory != nil }
    var usesDemoData: Bool { demo || demoLarge || isSnapshot }
    var publishesWidgets: Bool { !isSnapshot && (widgetsRequested || (profileDirectory.isEmpty && !usesDemoData)) }
}
