import AppKit

/// A reusable cards-view item hosting one `TransferCardView`.
@MainActor
final class TransferCardItem: NSCollectionViewItem {
    static let identifier = NSUserInterfaceItemIdentifier("TransferCard")

    var card: TransferCardView {
        // `loadView` always installs a card view.
        view as? TransferCardView ?? TransferCardView()
    }

    #if DEBUG
    /// Card views created so far (scripted checks of view reuse).
    static var loadedViews = 0
    #endif

    override func loadView() {
        view = TransferCardView(frame: .zero)
        #if DEBUG
        Self.loadedViews += 1
        #endif
    }

    override var isSelected: Bool {
        didSet { card.isSelected = isSelected }
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        card.isMenuTarget = false
    }
}
