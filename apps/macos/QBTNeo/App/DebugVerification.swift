#if DEBUG
import AppKit

/// Runtime verification aids for debug builds, driven by environment
/// variables so a scripted run can exercise the real app:
/// * `QBT_VERIFY_SEARCH=1`: open the Search window shortly after launch.
/// * `QBT_UI_SCRIPT=<scenario>`: run a scripted scenario through the real UI
///   with synthesized events (see `UIDriver` and `UIScenarios`).
@MainActor
enum DebugVerification {
    static func install(model: AppModel) -> Timer? {
        Symbol.verifyAll()
        CommandRegistry.verify()
        let environment = ProcessInfo.processInfo.environment
        if environment["QBT_VERIFY_SEARCH"] == "1" {
            Task {
                try? await Task.sleep(for: .seconds(6))
                model.show(pane: .search)
            }
        }
        if let scenario = environment["QBT_UI_SCRIPT"] {
            // Scripted runs must not disturb the user: windows stay practically invisible and click-through,
            // and refreshing must not depend on window occlusion.
            UIDriver.hidesWindows = true
            Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { _ in
                MainActor.assumeIsolated {
                    UIDriver.applyHiding()
                    if model.mainWindow?.isVisible == true { model.isMainWindowVisible = true }
                }
            }
            Task { await UIScenarios.run(scenario, driver: UIDriver(model: model)) }
        }
        return nil
    }
}
#endif
