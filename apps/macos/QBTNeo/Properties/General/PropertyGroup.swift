import SwiftUI

/// A titled label/value grid of the General tab (Transfer, Connection, …).
struct PropertyGroup<Content: View>: View {
    let title: LocalizedStringKey
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.tight) {
            Text(title)
                .font(.subheadline)
                .bold()
                .foregroundStyle(.secondary)
                .accessibilityAddTraits(.isHeader)
            Grid(alignment: .leading, horizontalSpacing: Design.Spacing.small, verticalSpacing: Design.Spacing.hairline) {
                content
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
