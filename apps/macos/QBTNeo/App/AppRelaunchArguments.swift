import Foundation

/// The `/bin/sh` arguments that open the app again once process `pid` has
/// exited, with the same launch arguments (`AppRelauncher`). Pure, so it is
/// unit-tested: paths and arguments travel as positional parameters, never
/// inside the script text.
enum AppRelaunchArguments {
    static let script = "while /bin/kill -0 \"$1\" 2>/dev/null; do /bin/sleep 0.2; done; shift; exec /usr/bin/open -n \"$@\""

    static func make(pid: Int32, bundlePath: String, launchArguments: [String]) -> [String] {
        var arguments = ["-c", script, "relaunch", String(pid), bundlePath]
        if !launchArguments.isEmpty {
            arguments.append("--args")
            arguments.append(contentsOf: launchArguments)
        }
        return arguments
    }
}
