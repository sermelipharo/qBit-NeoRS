import SwiftUI
import WidgetKit

/// A centered symbol, title and message (app not running, updates off,
/// nothing matching the filter).
struct WidgetNotice: View {
    let content: WidgetNoticeContent
    var compact = false

    var body: some View {
        VStack(spacing: compact ? 3 : 6) {
            Image(systemName: content.symbol)
                .font(.system(size: compact ? 18 : 26, weight: .regular))
                .foregroundStyle(.secondary)
                .widgetAccentable()
            Text(content.title)
                .font(.system(size: compact ? 12 : 13, weight: .semibold))
            if !content.message.isEmpty {
                Text(content.message)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
        }
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}
