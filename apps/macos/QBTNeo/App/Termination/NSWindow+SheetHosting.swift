import AppKit

extension NSWindow: SheetHosting {
    var hostedSheet: (any SheetHosting)? { attachedSheet }

    func endHostedSheet(_ sheet: any SheetHosting) {
        guard let sheet = sheet as? NSWindow else { return }
        endSheet(sheet, returnCode: .cancel)
    }
}
