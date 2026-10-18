#if DEBUG
import AppKit
import Darwin
import SwiftUI

/// Scenario "mem-window": the footprint cost of one auxiliary window with
/// content (`QBT_MEM_WINDOW` = search, rss or log). Logs the process
/// footprint before opening it and after its lists are populated; meant for
/// `--demo` runs, so searches and feeds stay local.
extension UIScenarios {
    static func memWindow(_ d: UIDriver) async throws {
        try await waitRunning(d)
        await d.pause(6)
        let name = ProcessInfo.processInfo.environment["QBT_MEM_WINDOW"] ?? "search"
        guard let id = AppWindowID(rawValue: name) else { throw UIDriver.Failure(description: "unknown window \(name)") }
        if id == .log {
            // A full window of messages of every level.
            let levels: [LogLevel] = [.normal, .info, .warning, .critical]
            for index in 0 ..< 80 {
                d.model.appendLocalLog("Measurement line \(index): the quick brown fox jumps over the lazy dog", level: levels[index % 4])
            }
            await d.pause(1)
        }
        let before = footprint()
        // A transparent window that is never activated, like the snapshot renderer's: the scenario does not
        // depend on the main window and the user's work is never covered.
        let content: AnyView = switch id {
        case .search: AnyView(SearchWindow(initialPattern: ProcessInfo.processInfo.environment["QBT_UI_QUERY"] ?? "night of the living dead"))
        case .rss: AnyView(RSSWindow(initialSelection: RSSNode.unreadID))
        case .log: AnyView(ExecutionLogWindow())
        default: throw UIDriver.Failure(description: "no content for \(name)")
        }
        let controller = NSHostingController(rootView: content.environment(d.model).environment(d.model.gui)
            .frame(width: 1_180, height: 680))
        let window = NSWindow(contentViewController: controller)
        window.styleMask = [.titled, .closable, .resizable]
        window.alphaValue = 0
        window.ignoresMouseEvents = true
        window.isReleasedWhenClosed = false
        window.setContentSize(CGSize(width: 1_180, height: 680))
        window.orderFrontRegardless()
        if id == .search {
            _ = await d.wait(timeout: 60) { windowTexts(d, window).contains("Search has finished") }
        }
        await d.pause(8)
        defer { window.close() }
        let tables = d.views(in: window, classNameContains: "TableView").compactMap { $0 as? NSTableView }
        d.log("tables: " + tables.map { "\(type(of: $0)) rows=\($0.numberOfRows) cols=\($0.numberOfColumns)" }.joined(separator: "; "))
        let after = footprint()
        d.log(String(format: "MEM window=%@ before=%.1fMB after=%.1fMB delta=%.1fMB", name, before, after, after - before))
    }

    /// The process' physical footprint in MB (what Activity Monitor shows as Memory).
    static func footprint() -> Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let result = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        return result == KERN_SUCCESS ? Double(info.phys_footprint) / 1_048_576 : -1
    }
}
#endif
