import Foundation

extension Array {
    /// The sort order after choosing `key` as the main sort key, the way a
    /// click on another table header does: `key` moves to the front and the
    /// former keys follow without their duplicate of it. Choosing the current
    /// main key again leaves the order (and its direction) unchanged.
    func promotingSortKey(_ key: Element, sameKey: (Element, Element) -> Bool) -> [Element] {
        if let first, sameKey(first, key) { return self }
        return [key] + filter { !sameKey($0, key) }
    }
}
