import AppKit

/// A flow layout of cards whose column count follows the width: as many
/// columns as fit with cards between `minWidth` and `maxWidth`, the cards
/// stretched to fill the row. Heights come from the delegate
/// (`TransferCardsController`): the cards of a row share the tallest one's.
@MainActor
final class TransferCardsLayout: NSCollectionViewFlowLayout {
    private(set) var columnCount = 1
    private(set) var cardWidth = TransferCardMetrics.minWidth
    private var preparedWidth: CGFloat = 0

    override init() {
        super.init()
        let spacing = TransferCardMetrics.spacing
        minimumInteritemSpacing = spacing
        minimumLineSpacing = spacing
        sectionInset = NSEdgeInsets(top: spacing, left: spacing, bottom: spacing, right: spacing)
        scrollDirection = .vertical
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Columns and card width for a content width (pure; unit-testable).
    static func columns(for width: CGFloat) -> (count: Int, itemWidth: CGFloat) {
        let spacing = TransferCardMetrics.spacing
        let available = max(width - 2 * spacing, TransferCardMetrics.minWidth)
        var count = max(1, Int(ceil((available + spacing) / (TransferCardMetrics.maxWidth + spacing))))
        var itemWidth = (available - CGFloat(count - 1) * spacing) / CGFloat(count)
        while count > 1, itemWidth < TransferCardMetrics.minWidth {
            count -= 1
            itemWidth = (available - CGFloat(count - 1) * spacing) / CGFloat(count)
        }
        return (count, floor(itemWidth))
    }

    /// The width the layout is prepared for (the scroll view's visible width).
    var contentWidth: CGFloat {
        guard let collectionView else { return 0 }
        return collectionView.enclosingScrollView?.contentView.bounds.width ?? collectionView.bounds.width
    }

    override func prepare() {
        preparedWidth = contentWidth
        let columns = Self.columns(for: preparedWidth)
        columnCount = columns.count
        cardWidth = columns.itemWidth
        super.prepare()
    }

    override func shouldInvalidateLayout(forBoundsChange newBounds: NSRect) -> Bool {
        newBounds.width != preparedWidth || super.shouldInvalidateLayout(forBoundsChange: newBounds)
    }}
