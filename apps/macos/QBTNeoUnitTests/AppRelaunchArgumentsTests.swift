import Foundation
import Testing

struct AppRelaunchArgumentsTests {
    @Test func passesPathsAndArgumentsAsParameters() {
        let args = AppRelaunchArguments.make(pid: 4242, bundlePath: "/Applications/qBit NeoRS.app", launchArguments: ["--profile", "/tmp/a b; rm -rf ~"])
        #expect(args == ["-c", AppRelaunchArguments.script, "relaunch", "4242", "/Applications/qBit NeoRS.app", "--args", "--profile", "/tmp/a b; rm -rf ~"])
        // Nothing user-controlled is ever part of the script text.
        #expect(!AppRelaunchArguments.script.contains("qBit"))
    }

    @Test func noLaunchArgumentsMeansNoArgsFlag() {
        let args = AppRelaunchArguments.make(pid: 1, bundlePath: "/A.app", launchArguments: [])
        #expect(args.last == "/A.app")
        #expect(!args.contains("--args"))
    }

    /// The script really waits for the process, then runs its command with
    /// the parameters intact (with `echo` in place of `open`).
    @Test func scriptWaitsForTheProcessThenRunsTheCommand() throws {
        // A process that isn't our child (no zombie to keep `kill -0` true): launchd adopts the sleep.
        let starter = Process()
        starter.executableURL = URL(filePath: "/bin/sh")
        starter.arguments = ["-c", "/bin/sleep 0.5 >/dev/null 2>&1 & echo $!"]
        let pidPipe = Pipe()
        starter.standardOutput = pidPipe
        try starter.run()
        starter.waitUntilExit()
        let pidText = String(data: pidPipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let pid = try #require(Int32(pidText.trimmingCharacters(in: .whitespacesAndNewlines)))
        let started = Date.now
        let script = AppRelaunchArguments.script.replacingOccurrences(of: "/usr/bin/open -n", with: "/bin/echo")
        var args = AppRelaunchArguments.make(pid: pid, bundlePath: "/A b.app", launchArguments: ["--x"])
        args[1] = script
        let shell = Process()
        shell.executableURL = URL(filePath: "/bin/sh")
        shell.arguments = args
        let pipe = Pipe()
        shell.standardOutput = pipe
        try shell.run()
        shell.waitUntilExit()
        #expect(Date.now.timeIntervalSince(started) > 0.3, "waited for the process to exit")
        let output = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)
        #expect(output == "/A b.app --args --x\n")
    }
}
