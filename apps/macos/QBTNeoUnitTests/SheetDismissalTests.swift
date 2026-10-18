import Testing

@MainActor
struct SheetDismissalTests {
    /// The names of the sheets ended, in order.
    final class Recorder {
        var ended: [String] = []
    }

    /// A window double: `endHostedSheet` detaches the sheet unless it is stubborn.
    final class FakeWindow: SheetHosting {
        let name: String
        let recorder: Recorder
        var sheet: FakeWindow?
        var stubborn = false

        init(_ name: String, _ recorder: Recorder) {
            self.name = name
            self.recorder = recorder
        }

        var hostedSheet: (any SheetHosting)? { sheet }

        func endHostedSheet(_ sheet: any SheetHosting) {
            guard let sheet = sheet as? FakeWindow, sheet === self.sheet else { return }
            recorder.ended.append(sheet.name)
            if !sheet.stubborn { self.sheet = nil }
        }
    }

    private let recorder = Recorder()

    private func window(_ name: String) -> FakeWindow {
        FakeWindow(name, recorder)
    }

    @Test func noSheetsNothingToDo() {
        let windows = [window("main"), window("settings")]
        #expect(!SheetDismissal.hasSheets(windows))
        #expect(SheetDismissal.closeAll(windows) == 0)
    }

    @Test func endsTheSheetsOfEveryWindow() {
        let main = window("main")
        main.sheet = window("speed limits")
        let settings = window("settings")
        settings.sheet = window("watched folder")
        #expect(SheetDismissal.hasSheets([main, settings]))
        #expect(SheetDismissal.closeAll([main, settings]) == 2)
        #expect(recorder.ended == ["speed limits", "watched folder"])
        #expect(!SheetDismissal.hasSheets([main, settings]))
    }

    @Test func endsTheInnermostSheetFirst() {
        let main = window("main")
        let dialog = window("dialog")
        let alert = window("alert")
        main.sheet = dialog
        dialog.sheet = alert
        #expect(SheetDismissal.closeAll([main]) == 2)
        #expect(recorder.ended == ["alert", "dialog"])
        #expect(main.sheet == nil)
    }

    @Test func aSheetThatWillNotEndDoesNotLoopForever() {
        let main = window("main")
        let stuck = window("stuck")
        stuck.stubborn = true
        main.sheet = stuck
        let other = window("other")
        other.sheet = window("other sheet")
        #expect(SheetDismissal.closeAll([main, other]) == 2)
        #expect(recorder.ended == ["stuck", "other sheet"])
    }

    @Test func stopsAtTheLimit() {
        let windows = (0 ..< 5).map { index in
            let host = window("w\(index)")
            host.sheet = window("s\(index)")
            return host
        }
        #expect(SheetDismissal.closeAll(windows, limit: 3) == 3)
    }
}
