import AppKit
import SwiftUI

/// The shortcut as an `NSMenuItem` key equivalent, matching what SwiftUI sets
/// for `keyboardShortcut` (Delete is the Backspace character, U+0008).
extension CommandShortcut {
    var menuKeyEquivalent: String {
        switch key {
        case .delete: "\u{8}"
        case .deleteForward: String(UnicodeScalar(UInt16(NSDeleteFunctionKey)).map(Character.init) ?? "\u{7F}")
        case .upArrow: String(UnicodeScalar(UInt16(NSUpArrowFunctionKey)).map(Character.init) ?? " ")
        case .downArrow: String(UnicodeScalar(UInt16(NSDownArrowFunctionKey)).map(Character.init) ?? " ")
        case .leftArrow: String(UnicodeScalar(UInt16(NSLeftArrowFunctionKey)).map(Character.init) ?? " ")
        case .rightArrow: String(UnicodeScalar(UInt16(NSRightArrowFunctionKey)).map(Character.init) ?? " ")
        case .return: "\r"
        case .escape: "\u{1B}"
        case .tab: "\t"
        case .space: " "
        default: String(key.character)
        }
    }

    var menuModifierMask: NSEvent.ModifierFlags {
        var mask: NSEvent.ModifierFlags = []
        if modifiers.contains(.command) { mask.insert(.command) }
        if modifiers.contains(.shift) { mask.insert(.shift) }
        if modifiers.contains(.option) { mask.insert(.option) }
        if modifiers.contains(.control) { mask.insert(.control) }
        return mask
    }
}
