import SwiftUI

/// The main window's Hide/Show Sidebar toolbar button, in place of the one
/// `NavigationSplitView` adds. SwiftUI's own item draws its symbol white in
/// View › Customize Toolbar… (macOS 27, light appearance, any app); this one
/// is an ordinary toolbar button, so the palette shows it like the others.
/// Same titles (from SwiftUI's table, so localized as before), symbol and
/// place next to the window buttons.
struct SidebarToggleButton: View {
    let gui: GUIPreferences

    private var title: String {
        SystemMenuTitle.text(gui.filtersSidebarVisible ? "Hide Sidebar" : "Show Sidebar", table: "Localizable")
    }

    var body: some View {
        Button(title, systemImage: Symbol.sidebarLeft) {
            withAnimation { gui.filtersSidebarVisible.toggle() }
        }
        .help(title)
    }
}
