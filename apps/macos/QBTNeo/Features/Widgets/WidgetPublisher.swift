import Foundation
import WidgetKit

/// Feeds the desktop and Notification Center widgets.
///
/// Costs nothing while no widget is installed: the app asks WidgetKit for
/// the installed widgets at launch and every 30 minutes, and a widget that
/// builds a timeline announces itself with a Darwin notification. While
/// widgets exist, `tick()` runs inside the existing refresh loop (no timer of
/// its own): it samples the speeds every 30 s and hands a freshly built
/// snapshot to `WidgetPublishPolicy`, which limits file writes to changes and
/// timeline reloads to a few per hour.
///
/// Torrents chosen in installed Torrent widgets are "pinned": published with
/// more detail and a speed history of their own (sampled on the same 30 s
/// rhythm), and only they accept the Torrent widget's start/stop commands.
///
/// The widgets' buttons arrive as Darwin notifications (Torrent widgets queue
/// a command file first, `WidgetCommandQueue`) and are honoured only while
/// widgets are installed, updates are on and the UI is not locked.
@MainActor
final class WidgetPublisher {
    static let configurationCheckInterval: TimeInterval = 30 * 60
    /// Minimum time between configuration checks prompted by widgets.
    static let configurationRecheckSpacing: TimeInterval = 10
    /// How long after a command or a newly found widget publishing ignores the usual intervals.
    static let urgentWindow: TimeInterval = 4
    /// Minimum time between two identical commands.
    static let commandSpacing: TimeInterval = 1

    private unowned let app: AppModel
    private var policy = WidgetPublishPolicy()
    private var history = WidgetSpeedHistory()
    private var listener: WidgetMessageListener?
    private var hasWidgets = false
    /// `--widgets`: publish whether or not a widget is installed (testing).
    private let alwaysPublishes: Bool
    /// Every filter, plus the filter-and-category pairs installed widgets use.
    private var scopes = WidgetTorrentSelector.scopes(configured: [])
    /// Torrents installed Torrent widgets follow, and their speed histories.
    private var pinnedIDs: [String]
    private var pinnedHistory: [String: WidgetSpeedHistory] = [:]
    private var commandValidator = WidgetCommandValidator()
    /// A widget asked for a configuration check while checks were spaced out.
    private var configurationCheckDue = false
    private var isCheckingConfigurations = false
    private var lastConfigurationCheck: TimeInterval?
    private var urgentUntil: TimeInterval = 0
    private var lastCommands: [WidgetMessage: TimeInterval] = [:]
    /// "Update widgets" as last seen by `tick()`.
    private var updatesEnabled: Bool

    init(app: AppModel) {
        self.app = app
        updatesEnabled = app.gui.updateWidgets
        alwaysPublishes = app.launch.widgetsRequested
        hasWidgets = alwaysPublishes
        pinnedIDs = WidgetTorrentSelector.pinnedIDs(app.launch.widgetPins)
    }

    /// Starts listening to the widgets and looks for installed ones.
    func start() {
        listener = WidgetMessageListener { [weak self] message in self?.receive(message) }
        checkConfigurations()
    }

    /// Publishing is wanted: widgets exist and "Update widgets" is on.
    var isActive: Bool { hasWidgets && app.gui.updateWidgets }

    /// Whether the next tick will build a snapshot, so a refresh with the main
    /// window hidden should fetch the transfer list first.
    var wantsTransferList: Bool {
        isActive && policy.shouldEvaluate(now: Self.uptime, forced: Self.uptime < urgentUntil)
    }

    /// One refresh-loop tick, after the stores were updated.
    func tick() {
        let now = Self.uptime
        if let last = lastConfigurationCheck {
            let since = now - last
            if since >= Self.configurationCheckInterval || (configurationCheckDue && since >= Self.configurationRecheckSpacing) {
                checkConfigurations()
            }
        }
        if app.gui.updateWidgets != updatesEnabled {
            updatesEnabled = app.gui.updateWidgets
            updatesSettingChanged()
        }
        guard isActive, app.phase == .running else { return }
        let time = Int64(Date.now.timeIntervalSince1970)
        history.record(download: app.status.downloadRate, upload: app.status.uploadRate, at: time)
        for id in pinnedIDs {
            guard let item = app.transfers.item(id) else { continue }
            pinnedHistory[id, default: WidgetSpeedHistory()].record(download: item.dlSpeed, upload: item.upSpeed, at: time)
        }
        let urgent = now < urgentUntil
        guard policy.shouldEvaluate(now: now, forced: urgent) else { return }
        let snapshot = WidgetSnapshotBuilder.build(
            transfers: app.transfers, names: app.names, status: app.status, history: history.samples, scopes: scopes, pinned: pinnedIDs,
            pinnedHistory: pinnedHistory.mapValues(\.samples), appStatus: app.lock.isLocked ? .locked : .running
        )
        switch policy.decide(snapshot.fingerprint, now: now, forced: urgent) {
        case .none:
            break
        case .write:
            save(snapshot, reload: false)
        case .writeAndReload:
            save(snapshot, reload: true)
        }
    }

    /// Publishes on the next tick regardless of the usual intervals (lock,
    /// settings changes, widget commands).
    func publishSoon() {
        guard isActive else { return }
        urgentUntil = Self.uptime + Self.urgentWindow
        app.refreshSoon()
    }

    /// "Update widgets" was switched; turning it off leaves a notice instead of stale data.
    private func updatesSettingChanged() {
        guard hasWidgets else { return }
        policy.reset()
        if app.gui.updateWidgets {
            publishSoon()
        } else {
            history.removeAll()
            pinnedHistory.removeAll()
            save(WidgetSnapshot(generatedAt: .now, status: .updatesOff), reload: true)
        }
    }

    /// The app is quitting: widgets show that it is not running.
    func publishTermination() {
        listener?.cancel()
        guard isActive else { return }
        save(WidgetSnapshot(generatedAt: .now, status: .notRunning), reload: true)
    }

    // MARK: Private

    private static var uptime: TimeInterval { ProcessInfo.processInfo.systemUptime }

    private func save(_ snapshot: WidgetSnapshot, reload: Bool) {
        do {
            try WidgetSnapshotStore.save(snapshot)
        } catch {
            return
        }
        if reload { WidgetCenter.shared.reloadAllTimelines() }
    }

    private func checkConfigurations() {
        guard !isCheckingConfigurations else { return }
        isCheckingConfigurations = true
        configurationCheckDue = false
        lastConfigurationCheck = Self.uptime
        Task {
            let configurations = (try? await WidgetCenter.shared.currentConfigurations()) ?? []
            isCheckingConfigurations = false
            let configured = configurations.compactMap { info -> WidgetScope? in
                guard info.kind == WidgetKind.transfers else { return nil }
                return info.widgetConfigurationIntent(of: TransfersConfigurationIntent.self)?.scope
            }
            let pins = configurations.compactMap { info -> String? in
                guard info.kind == WidgetKind.torrent else { return nil }
                return info.widgetConfigurationIntent(of: TorrentWidgetIntent.self)?.torrent?.id
            }
            let scopes = WidgetTorrentSelector.scopes(configured: configured)
            let pinned = WidgetTorrentSelector.pinnedIDs(pins + app.launch.widgetPins)
            let changed = scopes != self.scopes || pinned != pinnedIDs
            self.scopes = scopes
            pinnedIDs = pinned
            pinnedHistory = pinnedHistory.filter { pinned.contains($0.key) }
            if !setHasWidgets(!configurations.isEmpty), changed { publishSoon() }
        }
    }

    /// Returns whether the value changed.
    @discardableResult
    private func setHasWidgets(_ value: Bool) -> Bool {
        let value = value || alwaysPublishes
        guard value != hasWidgets else { return false }
        hasWidgets = value
        policy.reset()
        if value {
            publishSoon()
        } else {
            history.removeAll()
            pinnedHistory.removeAll()
        }
        return true
    }

    private func receive(_ message: WidgetMessage) {
        if message == .timelineRequested {
            // A first widget, or one that was just configured. Our own reloads
            // trigger this too, hence the spacing of configuration checks.
            if !hasWidgets { setHasWidgets(true) }
            if let last = lastConfigurationCheck, Self.uptime - last < Self.configurationRecheckSpacing {
                configurationCheckDue = true
                return
            }
            checkConfigurations()
            return
        }
        if message == .torrentCommand {
            runTorrentCommands()
            return
        }
        guard isActive, app.phase == .running, !app.lock.isLocked else { return }
        let now = Self.uptime
        if let last = lastCommands[message], now - last < Self.commandSpacing { return }
        lastCommands[message] = now
        switch message {
        case .stopAll: app.stopAll()
        case .startAll: app.startAll()
        case .altSpeedOn: app.setAltSpeedLimits(true)
        case .altSpeedOff: app.setAltSpeedLimits(false)
        case .timelineRequested, .torrentCommand: break
        }
        publishSoon()
    }

    /// Drains the command queue (always, so it never grows) and runs the
    /// commands `WidgetCommandValidator` accepts.
    private func runTorrentCommands() {
        let commands = WidgetCommandQueue.drain()
        guard isActive, app.phase == .running, !app.lock.isLocked, !commands.isEmpty else { return }
        let allowed = Set(pinnedIDs.filter { app.transfers.item($0) != nil })
        let accepted = commandValidator.accept(commands, now: .now, uptime: Self.uptime, allowed: allowed)
        guard !accepted.isEmpty else { return }
        let starts = accepted.filter { $0.action == .start }.map(\.torrentID)
        let stops = accepted.filter { $0.action == .stop }.map(\.torrentID)
        app.start(starts)
        app.stop(stops)
        publishSoon()
    }
}
