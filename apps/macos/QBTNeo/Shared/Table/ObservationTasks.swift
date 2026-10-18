import Observation

/// Drives AppKit views from `@Observable` models: each `observe` call runs
/// `apply` with the current value, then again after every transaction that
/// changes it (`Observations`, coalesced per main-actor turn). Owners call
/// `cancelAll()` when their view goes away.
@MainActor
final class ObservationTasks {
    private var tasks: [Task<Void, Never>] = []

    func observe<Value: Sendable>(
        _ value: @escaping @MainActor @Sendable () -> Value,
        apply: @escaping @MainActor (Value) -> Void
    ) {
        let values = Observations(value)
        tasks.append(Task { @MainActor in
            for await next in values {
                guard !Task.isCancelled else { return }
                apply(next)
            }
        })
    }

    func cancelAll() {
        for task in tasks { task.cancel() }
        tasks.removeAll()
    }
}
