use std::fmt;
use std::time::Duration;

/// Error classes, mapped like libtorrent errors are mapped to qBittorrent
/// tracker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackerErrorKind {
    /// The tracker answered with a `failure reason` (HTTP) or an error action
    /// (UDP). Shown as "Tracker error".
    TrackerFailure,
    /// The announce was not attempted on this endpoint, e.g. the tracker has
    /// no address of the endpoint's family (libtorrent `announce_skipped`).
    /// Shown as "Unreachable".
    Skipped,
    /// Anything else: DNS or connection failure, timeout, HTTP error status,
    /// malformed response, blocked by policy. Shown as "Not working".
    Other,
}

/// A failed announce or scrape. `message` is safe to display and log: it never
/// contains the tracker URL (which may embed a passkey).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerError {
    pub kind: TrackerErrorKind,
    pub message: String,
    /// The tracker asked us to wait at least this long before retrying.
    pub retry_in: Option<Duration>,
    /// The tracker must not be contacted again (HTTP 410, BEP 31 "never").
    pub never_retry: bool,
}

impl TrackerError {
    pub fn new(kind: TrackerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_in: None,
            never_retry: false,
        }
    }

    pub fn tracker_failure(message: impl Into<String>) -> Self {
        Self::new(TrackerErrorKind::TrackerFailure, message)
    }

    pub fn skipped(message: impl Into<String>) -> Self {
        Self::new(TrackerErrorKind::Skipped, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(TrackerErrorKind::Other, message)
    }

    pub fn permanent(mut self) -> Self {
        self.never_retry = true;
        self
    }
}

impl fmt::Display for TrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TrackerError {}
