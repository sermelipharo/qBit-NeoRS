import Foundation
import Security
import Sparkle

/// Updates from the GitHub releases (Sparkle): the appcast at `SUFeedURL`
/// lists the latest release, signed with the EdDSA key in `SUPublicEDKey`.
/// "Check for program updates" turns the daily background check on or off.
///
/// Only release downloads update themselves. Debug builds, demo and snapshot
/// runs, other profiles (unless `--updates`) and builds signed with a
/// developer team (which carry the widgets' App Group) are left alone: an
/// update would replace them with the ad-hoc signed download.
@MainActor
final class AppUpdater {
    private let controller: SPUStandardUpdaterController?
    private let observations = ObservationTasks()

    init(launch: LaunchOptions, gui: GUIPreferences) {
        guard Self.updatesItself(launch) else {
            controller = nil
            return
        }
        let controller = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: nil, userDriverDelegate: nil)
        self.controller = controller
        // Before starting, so Sparkle doesn't ask on the second launch: the setting already answers.
        controller.updater.automaticallyChecksForUpdates = gui.checkForUpdates
        controller.startUpdater()
        observations.observe({ gui.checkForUpdates }, apply: { [weak controller] in
            controller?.updater.automaticallyChecksForUpdates = $0
        })
    }

    /// Whether this copy updates itself (and shows "Check for Updates…").
    var isAvailable: Bool { controller != nil }

    var canCheckForUpdates: Bool { controller?.updater.canCheckForUpdates ?? false }

    func checkForUpdates() {
        controller?.checkForUpdates(nil)
    }

    private static func updatesItself(_ launch: LaunchOptions) -> Bool {
        #if DEBUG
        return false
        #else
        return !launch.usesDemoData && (launch.profileDirectory.isEmpty || launch.updatesRequested) && teamIdentifier() == nil
        #endif
    }

    /// The running app's signing team; nil when ad-hoc signed.
    private static func teamIdentifier() -> String? {
        var code: SecCode?
        guard SecCodeCopySelf([], &code) == errSecSuccess, let code else { return nil }
        var staticCode: SecStaticCode?
        guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess, let staticCode else { return nil }
        var info: CFDictionary?
        guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess,
              let info = info as? [String: Any] else { return nil }
        return info[kSecCodeInfoTeamIdentifier as String] as? String
    }
}
