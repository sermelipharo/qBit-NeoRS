//! Plugin versions (`PluginVersion = Utils::Version<2>` in qBittorrent 5.3):
//! exactly two dot-separated integers, none negative and at least one
//! above zero ("0.1", "1.0", "2.15"). Versions compare component-wise.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginVersion {
    pub major: i32,
    pub minor: i32,
}

impl PluginVersion {
    /// `Utils::Version::fromString` followed by `isValid()`: `None` for
    /// anything that is not a valid two-component version.
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.split('.');
        let major = parse_component(parts.next()?)?;
        let minor = parse_component(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let v = Self { major, minor };
        v.is_valid().then_some(v)
    }

    pub fn is_valid(&self) -> bool {
        self.major >= 0 && self.minor >= 0 && (self.major > 0 || self.minor > 0)
    }
}

/// `QStringView::toInt`: optional sign, decimal digits, surrounding
/// whitespace allowed; anything else fails.
fn parse_component(s: &str) -> Option<i32> {
    let t = s.trim();
    let digits = t.strip_prefix('+').unwrap_or(t);
    if digits.is_empty() || digits.starts_with('+') {
        return None;
    }
    t.parse::<i32>().ok()
}

impl fmt::Display for PluginVersion {
    /// `Utils::Version::toString` with two mandatory components.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_two_components() {
        assert_eq!(PluginVersion::parse("0.1"), Some(PluginVersion { major: 0, minor: 1 }));
        assert_eq!(PluginVersion::parse("1.0").unwrap().to_string(), "1.0");
        assert_eq!(PluginVersion::parse("2.15").unwrap().to_string(), "2.15");
        assert_eq!(PluginVersion::parse(" 3 . 04 ").unwrap().to_string(), "3.4");
        assert_eq!(PluginVersion::parse("+1.2").unwrap().to_string(), "1.2");
    }

    #[test]
    fn rejects_everything_else() {
        for bad in ["", "1", "1.2.3", "0.0", "-1.2", "1.-2", "a.b", "1.", ".1", "1..2", "++1.2", "1.2a", "99999999999.1"] {
            assert_eq!(PluginVersion::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn orders_component_wise() {
        let v = |s| PluginVersion::parse(s).unwrap();
        assert!(v("0.2") > v("0.1"));
        assert!(v("0.10") > v("0.9"));
        assert!(v("1.0") > v("0.99"));
        assert_eq!(v("1.01"), v("1.1"));
    }
}
