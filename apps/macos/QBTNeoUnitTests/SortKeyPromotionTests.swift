import Testing

struct SortKeyPromotionTests {
    private struct Key: Equatable {
        let name: String
        var ascending = true
    }

    private func promote(_ key: Key, in order: [Key]) -> [Key] {
        order.promotingSortKey(key) { $0.name == $1.name }
    }

    @Test func newKeyBecomesPrimaryAndKeepsTheFormerOnes() {
        let order = [Key(name: "name"), Key(name: "size", ascending: false)]
        #expect(promote(Key(name: "ratio"), in: order) == [Key(name: "ratio"), Key(name: "name"), Key(name: "size", ascending: false)])
    }

    @Test func secondaryKeyMovesToTheFrontWithoutDuplicate() {
        let order = [Key(name: "name"), Key(name: "size", ascending: false)]
        #expect(promote(Key(name: "size"), in: order) == [Key(name: "size"), Key(name: "name")])
    }

    @Test func currentPrimaryKeepsItsDirection() {
        let order = [Key(name: "size", ascending: false), Key(name: "name")]
        #expect(promote(Key(name: "size"), in: order) == order)
    }

    @Test func emptyOrderGetsTheKey() {
        #expect(promote(Key(name: "name"), in: []) == [Key(name: "name")])
    }
}
