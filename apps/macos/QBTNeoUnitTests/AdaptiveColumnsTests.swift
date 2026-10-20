import Testing

struct AdaptiveColumnsTests {
    @Test func everythingInOneRowWhenItFits() {
        let widths = AdaptiveColumns.widths(ideals: [100, 50, 80], width: 300, spacing: 10)
        // 230 natural + 20 spacing = 250; 50 left over, shared evenly.
        #expect(widths.count == 3)
        #expect(widths.map { ($0 * 100).rounded() / 100 } == [116.67, 66.67, 96.67])
    }

    @Test func naturalWidthsWithoutProposal() {
        #expect(AdaptiveColumns.widths(ideals: [100, 50, 80], width: nil, spacing: 10) == [100, 50, 80])
    }

    @Test func columnsTakeTheirWidestItem() {
        // 4 items in 2 columns: column 0 has items 0 and 2, column 1 items 1 and 3.
        let widths = AdaptiveColumns.widths(ideals: [100, 50, 120, 60], width: 200, spacing: 10)
        #expect(widths == [125, 65])
    }

    @Test func neverLeavesAnOrphanInTheLastRow() {
        // Four groups: 3 columns would fit but leave 3 + 1; use 2 + 2.
        let groups = AdaptiveColumns.widths(ideals: [100, 100, 100, 100], width: 350, spacing: 10)
        #expect(groups.count == 2)
        // Seven metrics: 6 or 5 columns leave 1 or 2 alone; use 4 + 3.
        let metrics = AdaptiveColumns.widths(ideals: Array(repeating: 100, count: 7), width: 650, spacing: 10)
        #expect(metrics.count == 4)
    }

    @Test func oneFullWidthColumnWhenNothingFits() {
        #expect(AdaptiveColumns.widths(ideals: [300, 300], width: 200, spacing: 10) == [200])
        #expect(AdaptiveColumns.widths(ideals: [], width: 200, spacing: 10).isEmpty)
    }
}
