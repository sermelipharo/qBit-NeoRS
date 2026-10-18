import AppKit

/// Opens this app again once the current process has exited (the restart
/// of an import). A small shell waits for our process id to disappear, then
/// runs `open -n` on our bundle with our launch arguments; it outlives us
/// as a child of launchd.
enum AppRelauncher {
    static func spawnAfterExit() {
        let process = Process()
        process.executableURL = URL(filePath: "/bin/sh")
        process.arguments = AppRelaunchArguments.make(
            pid: ProcessInfo.processInfo.processIdentifier,
            bundlePath: Bundle.main.bundleURL.path(percentEncoded: false),
            launchArguments: Array(CommandLine.arguments.dropFirst())
        )
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
    }
}
