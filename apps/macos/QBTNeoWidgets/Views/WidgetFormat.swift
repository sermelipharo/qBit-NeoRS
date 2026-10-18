import Foundation

/// Text formatting shared by the widget views (units follow the app's
/// `UnitFormatter`: binary units, floored values).
enum WidgetFormat {
    /// "12.4 MiB/s" split into "12.4" and "MiB/s", so the number can be set larger.
    static func speedParts(_ rate: Int64) -> (number: String, unit: String) {
        let text = UnitFormatter.speed(max(rate, 0))
        guard let space = text.firstIndex(of: "\u{00A0}") else { return (text, "") }
        return (String(text[..<space]), String(text[text.index(after: space)...]))
    }

    static func speed(_ rate: Int64) -> String {
        UnitFormatter.speed(max(rate, 0))
    }

    /// Whole percent, floored so 99.9 % never reads as 100 %.
    static func percent(_ progress: Double) -> String {
        let value = (min(max(progress, 0), 1) * 100).rounded(.down) / 100
        return value.formatted(.percent.precision(.fractionLength(0)))
    }

    /// "4h 12m", "35m", "50s".
    static func eta(_ seconds: Int64) -> String {
        let units: Set<Duration.UnitsFormatStyle.Unit> = seconds < 60 ? [.seconds] : [.days, .hours, .minutes]
        return Duration.seconds(seconds).formatted(.units(allowed: units, width: .narrow, maximumUnitCount: 2))
    }
}
