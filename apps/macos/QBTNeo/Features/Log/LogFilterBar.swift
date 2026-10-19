import SwiftUI

/// Bottom bar of the Execution Log: message-type checkboxes (persisted in
/// `GUI/Log/Types`, shared with View › Log), the line count and Clear.
struct LogFilterBar: View {
    /// In the main window: the General / Blocked IPs switch and a button to
    /// expand the bottom panel to the full window or back.
    struct Resize {
        let isFull: Bool
        let toggle: () -> Void
    }

    let model: ExecutionLogModel
    @Bindable var gui: GUIPreferences
    let clear: () -> Void
    var resize: Resize?

    var body: some View {
        HStack(spacing: Design.Spacing.large) {
            if resize != nil {
                LogTabPicker(model: model)
                    .controlSize(.small)
            }
            if model.tab == .general {
                Toggle("Normal", isOn: $gui[logType: LogLevel.normal.filterFlag])
                Toggle("Information", isOn: $gui[logType: LogLevel.info.filterFlag])
                    .foregroundStyle(FeatureDesign.Palette.logInfo)
                Toggle("Warning", isOn: $gui[logType: LogLevel.warning.filterFlag])
                    .foregroundStyle(FeatureDesign.Palette.logWarning)
                Toggle("Critical", isOn: $gui[logType: LogLevel.critical.filterFlag])
                    .foregroundStyle(FeatureDesign.Palette.logCritical)
            }
            Spacer()
            Text("^[\(model.visibleLines.count) entry](inflect: true)")
                .monospacedDigit()
                .foregroundStyle(.secondary)
            Button("Clear", action: clear)
                .disabled(model.visibleLines.isEmpty)
            if let resize {
                Button(resize.isFull ? "Show as Panel" : "Show Full Size",
                       systemImage: resize.isFull ? Symbol.arrowDownRightAndArrowUpLeft : Symbol.arrowUpLeftAndArrowDownRight,
                       action: resize.toggle)
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .help(resize.isFull ? "Show the log as a panel under the transfers" : "Show the log in the whole window")
            }
        }
        .toggleStyle(.checkbox)
        .controlSize(.small)
        .padding(.horizontal, Design.Spacing.medium)
        .padding(.vertical, Design.Spacing.small)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }
}
