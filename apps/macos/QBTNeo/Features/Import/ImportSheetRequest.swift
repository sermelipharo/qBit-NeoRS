import Foundation

/// Opens the import sheet: one source from the File menu, or the sources
/// found on the first launch (`isOffer`).
struct ImportSheetRequest: Identifiable {
    let sources: [ImportSource]
    var isOffer = false

    var id: String {
        (isOffer ? "offer-" : "") + sources.map(\.key).joined(separator: "-")
    }
}
