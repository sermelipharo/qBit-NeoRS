import Foundation
import notify

/// Darwin notifications from the widget extension to the app. They carry no
/// payload, so only this fixed set of requests can be expressed; the app
/// decides whether to act on them (see docs/SECURITY.md, "Widgets").
enum WidgetMessage: String, CaseIterable, Sendable {
    /// A widget built a timeline: at least one is installed (or one was reconfigured).
    case timelineRequested = "mk.lex.qbitneors.widget.timeline"
    case stopAll = "mk.lex.qbitneors.widget.stop-all"
    case startAll = "mk.lex.qbitneors.widget.start-all"
    case altSpeedOn = "mk.lex.qbitneors.widget.alt-speed-on"
    case altSpeedOff = "mk.lex.qbitneors.widget.alt-speed-off"
    /// A Torrent widget queued a command (`WidgetCommandQueue`).
    case torrentCommand = "mk.lex.qbitneors.widget.torrent-command"

    func post() {
        notify_post(rawValue)
    }
}
