import SwiftUI

/// Text selection for a General tab value, attached only once the tab allows
/// it (`propertiesValuesSelectable`). Every selectable `Text` carries its own
/// text interaction view, gesture recognizers and TextKit layout manager
/// (about 2 MB for the tab), which are useless until the pointer can reach the
/// values; selection works as before because the pointer always enters the
/// tab before a click.
struct SelectableValue: ViewModifier {
    @Environment(\.propertiesValuesSelectable) private var selectable

    func body(content: Content) -> some View {
        if selectable {
            content.textSelection(.enabled)
        } else {
            content
        }
    }
}
