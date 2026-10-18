import Foundation
import ObjectiveC

/// AppKit and SwiftUI autosave window frames, split-view divider positions,
/// table columns and status-item positions into `UserDefaults.standard`, which
/// is always the app's real domain (`mk.lex.qbitneors`). An instance with its
/// own store (`--profile`, demo, snapshot) would overwrite the owner's layout,
/// so it reroutes the standard object's primitive accessors to that store:
/// writes and removals go only there, and reads look there first, so such an
/// instance also keeps its own layout. The typed setters (`set(_: Int, ...)`
/// and friends) all end in `setObject:forKey:`. The default profile installs
/// nothing.
enum StandardDefaultsRedirect {
    private typealias Getter = @convention(c) (UserDefaults, Selector, NSString) -> AnyObject?
    private typealias Setter = @convention(c) (UserDefaults, Selector, AnyObject?, NSString) -> Void
    private typealias Remover = @convention(c) (UserDefaults, Selector, NSString) -> Void

    private static let setSelector = #selector(UserDefaults.set(_:forKey:) as (UserDefaults) -> (Any?, String) -> Void)
    private static let getSelector = #selector(UserDefaults.object(forKey:))
    private static let removeSelector = #selector(UserDefaults.removeObject(forKey:))

    /// Call once, before any window exists.
    @MainActor
    static func install(redirectingTo store: UserDefaults) {
        let standard = UserDefaults.standard
        guard store !== standard,
              let setMethod = class_getInstanceMethod(UserDefaults.self, setSelector),
              let getMethod = class_getInstanceMethod(UserDefaults.self, getSelector),
              let removeMethod = class_getInstanceMethod(UserDefaults.self, removeSelector)
        else { return }

        let originalSet = unsafeBitCast(method_getImplementation(setMethod), to: Setter.self)
        let set: @convention(block) (UserDefaults, AnyObject?, NSString) -> Void = { defaults, value, key in
            if defaults === standard {
                store.set(value, forKey: key as String)
            } else {
                originalSet(defaults, setSelector, value, key)
            }
        }
        let originalGet = unsafeBitCast(method_getImplementation(getMethod), to: Getter.self)
        let get: @convention(block) (UserDefaults, NSString) -> AnyObject? = { defaults, key in
            if defaults === standard, let value = store.object(forKey: key as String) {
                return value as AnyObject
            }
            return originalGet(defaults, getSelector, key)
        }
        let originalRemove = unsafeBitCast(method_getImplementation(removeMethod), to: Remover.self)
        let remove: @convention(block) (UserDefaults, NSString) -> Void = { defaults, key in
            if defaults === standard {
                store.removeObject(forKey: key as String)
            } else {
                originalRemove(defaults, removeSelector, key)
            }
        }
        method_setImplementation(setMethod, imp_implementationWithBlock(set))
        method_setImplementation(getMethod, imp_implementationWithBlock(get))
        method_setImplementation(removeMethod, imp_implementationWithBlock(remove))
    }
}
