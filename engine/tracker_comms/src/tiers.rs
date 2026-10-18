//! A torrent's tiered tracker list and its announce state machine.
//!
//! This is a port of libtorrent's per-torrent tracker logic
//! (`announce_with_tracker`, `update_tracker_timer`, `announce_infohash`):
//! * trackers are kept sorted by tier; within a tier they are tried in order,
//!   a tracker that fails moves to the back of its tier and one that answers
//!   moves to the front;
//! * a tracker counts as "working" while it has no consecutive failures;
//! * `announce_to_all_trackers` announces to every tracker of a tier,
//!   `announce_to_all_tiers` to every tier;
//! * failures back off as `5 + 12.5 * fails^2` seconds (libtorrent's default
//!   `tracker_backoff` of 250%), capped at one hour, or longer if the tracker
//!   asks for it.
//!
//! Announce state is kept per tracker; the per-address-family results of the
//! last announce are kept for display ([`TrackerEndpointStatus`]).

use std::time::{Duration, Instant, SystemTime};

use serde_derive::Serialize;
use url::Url;

use crate::client::{AddressFamily, AnnounceOutcome};
use crate::error::{TrackerError, TrackerErrorKind};
use crate::policy::{is_supported_scheme, parse_tracker_url};
use crate::response::ScrapeData;
use crate::tracker_comms_http::AnnounceEvent;

/// Maximum number of trackers per torrent.
pub const MAX_TRACKERS: usize = 1000;
/// Minimum time between two scrapes of the same tracker.
pub const SCRAPE_INTERVAL: Duration = Duration::from_secs(30 * 60);

const MAX_RETRY_DELAY: Duration = Duration::from_secs(3600);
/// Upper bound for any delay a tracker can impose (intervals, "retry in").
const MAX_INTERVAL: Duration = Duration::from_secs(7 * 24 * 3600);
const DEFAULT_MIN_INTERVAL: Duration = Duration::from_secs(30);

pub type TrackerId = u64;

/// Tracker state as shown in the Trackers tab (qBittorrent
/// `TrackerEndpointState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum TrackerState {
    NotContacted,
    Working,
    NotWorking,
    TrackerError,
    Unreachable,
}

/// Where a tracker entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum TrackerOrigin {
    /// The torrent's metadata or magnet link.
    Torrent,
    /// Session-wide extra trackers (dropped for private torrents).
    Session,
    /// Added through the API.
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerEditError {
    NotFound,
    InvalidUrl,
    AlreadyExists,
}

impl std::fmt::Display for TrackerEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TrackerEditError::NotFound => "tracker not found",
            TrackerEditError::InvalidUrl => "tracker URL is invalid",
            TrackerEditError::AlreadyExists => "tracker URL already exists",
        })
    }
}

impl std::error::Error for TrackerEditError {}

/// Settings that drive tracker selection and timing.
#[derive(Clone, Debug)]
pub struct AnnounceSettings {
    pub announce_to_all_trackers: bool,
    pub announce_to_all_tiers: bool,
    /// Lower bound for intervals returned by trackers.
    pub min_announce_interval: Duration,
    /// Use this interval instead of the tracker's (testing/debugging).
    pub force_interval: Option<Duration>,
}

impl Default for AnnounceSettings {
    fn default() -> Self {
        Self {
            announce_to_all_trackers: false,
            announce_to_all_tiers: true,
            min_announce_interval: Duration::from_secs(300),
            force_interval: None,
        }
    }
}

/// Per address family result of the last announce.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TrackerEndpointStatus {
    pub family: AddressFamily,
    pub state: TrackerState,
    pub updating: bool,
    pub message: String,
    /// Peers returned by the last announce.
    pub peers: Option<u32>,
    pub seeds: Option<u32>,
    pub leechers: Option<u32>,
    pub downloaded: Option<u32>,
    pub next_announce_in: Option<Duration>,
    pub min_announce_in: Option<Duration>,
}

/// Status of one tracker, aggregated over its endpoints the way qBittorrent
/// does (`updateTrackerEntryStatus`).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TrackerStatus {
    pub url: String,
    pub tier: u32,
    pub state: TrackerState,
    /// An announce is in progress.
    pub updating: bool,
    /// Failure reason, error text or warning message.
    pub message: String,
    /// Peers returned by the last announce.
    pub peers: Option<u32>,
    /// Seeders (announce `complete` or scrape).
    pub seeds: Option<u32>,
    /// Leechers (announce `incomplete` or scrape).
    pub leechers: Option<u32>,
    /// Times downloaded (announce `downloaded` or scrape).
    pub downloaded: Option<u32>,
    /// Time until the next announce; `None` when not scheduled.
    pub next_announce_in: Option<Duration>,
    /// Time until announcing is allowed again.
    pub min_announce_in: Option<Duration>,
    pub last_announce: Option<SystemTime>,
    pub endpoints: Vec<TrackerEndpointStatus>,
}

#[derive(Clone, Debug)]
struct EndpointInfo {
    family: AddressFamily,
    state: TrackerState,
    message: String,
    peers: Option<u32>,
    seeds: Option<u32>,
    leechers: Option<u32>,
    downloaded: Option<u32>,
}

#[derive(Clone, Debug)]
struct TrackerEntry {
    id: TrackerId,
    url: String,
    parsed: Url,
    tier: u32,
    origin: TrackerOrigin,
    trusted: bool,

    // 0 = unlimited.
    fail_limit: u8,
    fails: u8,
    verified: bool,
    updating: bool,
    start_sent: bool,
    complete_sent: bool,
    // Force announce to this tracker regardless of tier selection.
    force_bypass: bool,
    // None = as soon as possible.
    next_announce: Option<Instant>,
    min_announce: Option<Instant>,
    last_error: Option<TrackerErrorKind>,
    message: String,
    tracker_id: Option<Vec<u8>>,
    last_announce: Option<SystemTime>,
    endpoints: Vec<EndpointInfo>,

    scrape: Option<ScrapeData>,
    scrape_updating: bool,
    scrape_unsupported: bool,
    last_scrape: Option<Instant>,
}

impl TrackerEntry {
    fn new(
        id: TrackerId,
        url: String,
        parsed: Url,
        tier: u32,
        origin: TrackerOrigin,
        trusted: bool,
    ) -> Self {
        Self {
            id,
            url,
            parsed,
            tier,
            origin,
            trusted,
            fail_limit: 0,
            fails: 0,
            verified: false,
            updating: false,
            start_sent: false,
            complete_sent: false,
            force_bypass: false,
            next_announce: None,
            min_announce: None,
            last_error: None,
            message: String::new(),
            tracker_id: None,
            last_announce: None,
            endpoints: Vec::new(),
            scrape: None,
            scrape_updating: false,
            scrape_unsupported: false,
            last_scrape: None,
        }
    }

    fn is_working(&self) -> bool {
        self.fails == 0
    }

    fn fail_limited(&self) -> bool {
        self.fail_limit != 0 && self.fails >= self.fail_limit
    }

    fn can_announce(&self, now: Instant, is_seed: bool) -> bool {
        let need_send_complete = is_seed && !self.complete_sent;
        self.next_announce.is_none_or(|t| now >= t)
            && (self.min_announce.is_none_or(|t| now >= t) || need_send_complete)
            && !self.fail_limited()
            && !self.updating
    }

    fn clear_status(&mut self) {
        self.updating = false;
        self.scrape_updating = false;
        self.verified = false;
        self.fails = 0;
        self.force_bypass = false;
        self.next_announce = None;
        self.min_announce = None;
        self.last_error = None;
        self.message.clear();
        self.endpoints.clear();
        self.scrape = None;
        self.last_scrape = None;
    }
}

/// A planned announce, produced by [`TrackerList::plan`].
#[derive(Clone, Debug)]
pub struct PlannedAnnounce {
    pub id: TrackerId,
    pub url: Url,
    pub trusted: bool,
    pub event: AnnounceEvent,
    pub tracker_id: Option<Vec<u8>>,
}

/// A planned scrape, produced by [`TrackerList::plan_scrapes`].
#[derive(Clone, Debug)]
pub struct PlannedScrape {
    pub id: TrackerId,
    pub url: Url,
    pub trusted: bool,
}

/// Summary of an applied announce result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedAnnounce {
    pub success: bool,
}

#[derive(Debug, Default, Clone)]
pub struct TrackerList {
    entries: Vec<TrackerEntry>,
    next_id: TrackerId,
}

fn state_of(kind: TrackerErrorKind) -> TrackerState {
    match kind {
        TrackerErrorKind::TrackerFailure => TrackerState::TrackerError,
        TrackerErrorKind::Skipped => TrackerState::Unreachable,
        TrackerErrorKind::Other => TrackerState::NotWorking,
    }
}

fn error_rank(kind: TrackerErrorKind) -> u8 {
    match kind {
        TrackerErrorKind::TrackerFailure => 2,
        TrackerErrorKind::Other => 1,
        TrackerErrorKind::Skipped => 0,
    }
}

/// libtorrent's retry delay after `fails` consecutive failures.
pub fn retry_delay(fails: u8) -> Duration {
    let f = u64::from(fails);
    let secs = 5 + f * f * 5 * 250 / 100;
    Duration::from_secs(secs).min(MAX_RETRY_DELAY)
}

impl TrackerList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `(url, tier)` pairs in announce order.
    pub fn entries(&self) -> Vec<(String, u32)> {
        self.entries
            .iter()
            .map(|e| (e.url.clone(), e.tier))
            .collect()
    }

    pub fn contains(&self, url: &str) -> bool {
        let url = url.trim();
        self.entries.iter().any(|e| e.url == url)
    }

    /// Whether the tracker is trusted by SSRF mitigation (None: not listed).
    pub fn is_trusted(&self, url: &str) -> Option<bool> {
        let url = url.trim();
        self.entries.iter().find(|e| e.url == url).map(|e| e.trusted)
    }

    pub fn max_tier(&self) -> Option<u32> {
        self.entries.iter().map(|e| e.tier).max()
    }

    fn position(&self, id: TrackerId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    fn alloc_id(&mut self) -> TrackerId {
        self.next_id += 1;
        self.next_id
    }

    /// Add a tracker at `tier` (after the existing trackers of that tier).
    /// Returns false for duplicates, invalid URLs or when the list is full.
    pub fn add(&mut self, url: &str, tier: u32, origin: TrackerOrigin, trusted: bool) -> bool {
        let url = url.trim();
        if self.entries.len() >= MAX_TRACKERS || self.contains(url) {
            return false;
        }
        let Some(parsed) = parse_tracker_url(url) else {
            return false;
        };
        let id = self.alloc_id();
        let pos = self
            .entries
            .iter()
            .rposition(|e| e.tier <= tier)
            .map(|p| p + 1)
            .unwrap_or(0);
        self.entries.insert(
            pos,
            TrackerEntry::new(id, url.to_owned(), parsed, tier, origin, trusted),
        );
        true
    }

    /// Remove trackers by URL. Returns how many were removed.
    pub fn remove<S: AsRef<str>>(&mut self, urls: &[S]) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|e| !urls.iter().any(|u| u.as_ref().trim() == e.url));
        before - self.entries.len()
    }

    /// Remove all trackers of one origin.
    pub fn remove_origin(&mut self, origin: TrackerOrigin) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.origin != origin);
        before - self.entries.len()
    }

    /// Replace a tracker URL in place, keeping its tier. The new tracker
    /// starts from a clean state.
    pub fn edit(&mut self, old: &str, new: &str, trusted: bool) -> Result<(), TrackerEditError> {
        let old = old.trim();
        let new = new.trim();
        let pos = self
            .entries
            .iter()
            .position(|e| e.url == old)
            .ok_or(TrackerEditError::NotFound)?;
        let parsed = parse_tracker_url(new).ok_or(TrackerEditError::InvalidUrl)?;
        if old == new {
            return Ok(());
        }
        if self.contains(new) {
            return Err(TrackerEditError::AlreadyExists);
        }
        let id = self.alloc_id();
        let tier = self.entries[pos].tier;
        self.entries[pos] = TrackerEntry::new(
            id,
            new.to_owned(),
            parsed,
            tier,
            TrackerOrigin::User,
            trusted,
        );
        Ok(())
    }

    /// Replace the whole list. Trackers whose URL is kept retain their
    /// state; duplicates and invalid URLs are skipped.
    pub fn replace(&mut self, list: Vec<(String, u32)>, origin: TrackerOrigin, trusted: bool) {
        self.replace_with(list, origin, |_| trusted)
    }

    /// Like [`TrackerList::replace`], with the trust of each new URL given by
    /// `trusted` (kept URLs keep theirs).
    pub fn replace_with(
        &mut self,
        list: Vec<(String, u32)>,
        origin: TrackerOrigin,
        trusted: impl Fn(&str) -> bool,
    ) {
        let mut old = std::mem::take(&mut self.entries);
        let mut new: Vec<TrackerEntry> = Vec::with_capacity(list.len());
        for (url, tier) in list {
            let url = url.trim();
            if new.len() >= MAX_TRACKERS || new.iter().any(|e| e.url == url) {
                continue;
            }
            if let Some(pos) = old.iter().position(|e| e.url == url) {
                let mut e = old.swap_remove(pos);
                e.tier = tier;
                new.push(e);
            } else if let Some(parsed) = parse_tracker_url(url) {
                let id = self.alloc_id();
                new.push(TrackerEntry::new(
                    id,
                    url.to_owned(),
                    parsed,
                    tier,
                    origin,
                    trusted(url),
                ));
            }
        }
        new.sort_by_key(|e| e.tier);
        self.entries = new;
    }

    /// Request an announce as soon as possible, ignoring the minimum interval.
    /// With a URL, that tracker is announced to even if the tier selection
    /// would skip it. Returns false if the URL is not in the list.
    pub fn force_reannounce(&mut self, url: Option<&str>) -> bool {
        let mut found = false;
        for e in self.entries.iter_mut() {
            let matches = match url {
                None => true,
                Some(u) => e.url == u.trim(),
            };
            if matches {
                found = true;
                e.next_announce = None;
                e.min_announce = None;
                e.force_bypass = url.is_some();
            }
        }
        found
    }

    /// The torrent became a seed: announce `completed` as soon as possible.
    pub fn on_completed(&mut self) {
        for e in self.entries.iter_mut().filter(|e| !e.complete_sent) {
            e.next_announce = None;
            e.min_announce = None;
        }
    }

    /// The torrent turned out to be complete when its data was first checked
    /// (e.g. a magnet whose files already exist): like a torrent that starts
    /// as a seed, it never sends `completed` (libtorrent `files_checked`).
    pub fn mark_complete_sent(&mut self) {
        for e in self.entries.iter_mut() {
            e.complete_sent = true;
        }
    }

    /// Start a new announcing session (torrent started).
    pub fn reset_for_start(&mut self, is_seed: bool) {
        for e in self.entries.iter_mut() {
            e.clear_status();
            e.start_sent = false;
            // A torrent that starts as a seed never sends `completed`.
            e.complete_sent = is_seed;
        }
    }

    /// Announcing stopped: statuses go back to "not contacted".
    pub fn reset_after_stop(&mut self) {
        for e in self.entries.iter_mut() {
            e.clear_status();
            e.start_sent = false;
        }
    }

    /// In-flight requests were abandoned.
    pub fn cancel_updating(&mut self) {
        for e in self.entries.iter_mut() {
            e.updating = false;
            e.scrape_updating = false;
        }
    }

    fn start_announce(e: &mut TrackerEntry, now: Instant, is_seed: bool) -> PlannedAnnounce {
        let event = if !e.start_sent {
            AnnounceEvent::Started
        } else if !e.complete_sent && is_seed {
            AnnounceEvent::Completed
        } else {
            AnnounceEvent::None
        };
        e.updating = true;
        e.force_bypass = false;
        e.next_announce = Some(now);
        e.min_announce = Some(now);
        PlannedAnnounce {
            id: e.id,
            url: e.parsed.clone(),
            trusted: e.trusted,
            event,
            tracker_id: e.tracker_id.clone(),
        }
    }

    /// Select the trackers to announce to now (libtorrent
    /// `announce_with_tracker`) and mark them as updating.
    pub fn plan(
        &mut self,
        now: Instant,
        is_seed: bool,
        s: &AnnounceSettings,
    ) -> Vec<PlannedAnnounce> {
        let all_trackers = s.announce_to_all_trackers;
        let all_tiers = s.announce_to_all_tiers;
        let mut tier: Option<u32> = None;
        let mut sent = false;
        let mut done = false;
        let mut out = Vec::new();

        for e in self.entries.iter_mut() {
            if e.force_bypass && e.can_announce(now, is_seed) {
                out.push(Self::start_announce(e, now, is_seed));
                continue;
            }
            if done {
                continue;
            }
            if all_tiers && !all_trackers && sent && tier.is_some_and(|t| e.tier <= t) {
                continue;
            }
            if !all_tiers && sent && tier.is_some_and(|t| e.tier > t) {
                continue;
            }
            if e.is_working() {
                tier = Some(e.tier);
                sent = false;
            }
            if !e.can_announce(now, is_seed) {
                if e.is_working() {
                    sent = true;
                    if !all_trackers && !all_tiers {
                        done = true;
                    }
                }
                continue;
            }
            out.push(Self::start_announce(e, now, is_seed));
            sent = true;
            tier = Some(e.tier);
            if !all_trackers && !all_tiers {
                done = true;
            }
        }
        out
    }

    /// When the next announce is due (libtorrent `update_tracker_timer`).
    /// A time in the past means "now"; `None` means nothing is scheduled.
    pub fn next_wakeup(&self, now: Instant, s: &AnnounceSettings) -> Option<Instant> {
        let all_trackers = s.announce_to_all_trackers;
        let all_tiers = s.announce_to_all_tiers;
        let mut next: Option<Instant> = None;
        let mut tier: Option<u32> = None;
        let mut found_working = false;
        let mut done = false;
        let due = |e: &TrackerEntry| {
            let n = e.next_announce.unwrap_or(now);
            let m = e.min_announce.unwrap_or(now);
            n.max(m)
        };
        let mut consider = |t: Instant| {
            if next.is_none_or(|n| t < n) {
                next = Some(t);
            }
        };

        for e in self.entries.iter() {
            if e.force_bypass && !e.updating && !e.fail_limited() {
                consider(due(e));
            }
            if done {
                continue;
            }
            if all_tiers && found_working && tier.is_some_and(|t| e.tier <= t) {
                continue;
            }
            if !all_tiers && tier.is_some_and(|t| e.tier > t) {
                done = true;
                continue;
            }
            if e.is_working() {
                tier = Some(e.tier);
                found_working = false;
            }
            if e.fail_limited() {
                continue;
            }
            if e.updating {
                found_working = true;
            } else if !found_working || e.is_working() {
                consider(due(e));
            }
            if e.is_working() {
                found_working = true;
            }
            if found_working && !all_trackers && !all_tiers {
                done = true;
            }
        }
        next
    }

    /// Trackers to send `stopped` to: every tracker we announced `started` to.
    pub fn plan_stop(&mut self) -> Vec<PlannedAnnounce> {
        let mut out = Vec::new();
        for e in self.entries.iter_mut() {
            if e.start_sent && is_supported_scheme(&e.parsed) {
                e.updating = true;
                out.push(PlannedAnnounce {
                    id: e.id,
                    url: e.parsed.clone(),
                    trusted: e.trusted,
                    event: AnnounceEvent::Stopped,
                    tracker_id: e.tracker_id.clone(),
                });
            }
        }
        out
    }

    fn endpoint_infos(prev: &[EndpointInfo], outcome: &AnnounceOutcome) -> Vec<EndpointInfo> {
        outcome
            .endpoints
            .iter()
            .map(|ep| match &ep.result {
                Ok(r) => EndpointInfo {
                    family: ep.family,
                    state: TrackerState::Working,
                    message: r.warning_message.clone().unwrap_or_default(),
                    peers: Some(u32::try_from(r.peers.len()).unwrap_or(u32::MAX)),
                    seeds: r.complete,
                    leechers: r.incomplete,
                    downloaded: r.downloaded,
                },
                Err(e) => {
                    // Keep the last known numbers of this endpoint.
                    let old = prev.iter().find(|p| p.family == ep.family);
                    EndpointInfo {
                        family: ep.family,
                        state: state_of(e.kind),
                        message: e.message.clone(),
                        peers: old.and_then(|o| o.peers),
                        seeds: old.and_then(|o| o.seeds),
                        leechers: old.and_then(|o| o.leechers),
                        downloaded: old.and_then(|o| o.downloaded),
                    }
                }
            })
            .collect()
    }

    /// Move entry `idx` to the front of its tier.
    fn prioritize(&mut self, mut idx: usize) {
        while idx > 0 && self.entries[idx - 1].tier == self.entries[idx].tier {
            self.entries.swap(idx, idx - 1);
            idx -= 1;
        }
    }

    /// Move entry `idx` to the back of its tier.
    fn deprioritize(&mut self, mut idx: usize) {
        while idx + 1 < self.entries.len() && self.entries[idx + 1].tier == self.entries[idx].tier {
            self.entries.swap(idx, idx + 1);
            idx += 1;
        }
    }

    /// Record the result of an announce. Returns `None` if the tracker was
    /// removed meanwhile.
    pub fn apply_announce(
        &mut self,
        id: TrackerId,
        event: AnnounceEvent,
        outcome: &AnnounceOutcome,
        now: Instant,
        wall: SystemTime,
        s: &AnnounceSettings,
    ) -> Option<AppliedAnnounce> {
        let idx = self.position(id)?;
        let e = &mut self.entries[idx];
        e.updating = false;
        e.last_announce = Some(wall);
        e.endpoints = Self::endpoint_infos(&e.endpoints, outcome);

        let ok: Vec<_> = outcome
            .endpoints
            .iter()
            .filter_map(|ep| ep.result.as_ref().ok())
            .collect();

        if !ok.is_empty() {
            e.fails = 0;
            e.verified = true;
            e.last_error = None;
            e.message = ok
                .iter()
                .find_map(|r| r.warning_message.clone())
                .unwrap_or_default();
            if let Some(tid) = ok.iter().find_map(|r| r.tracker_id.clone()) {
                e.tracker_id = Some(tid);
            }
            let tracker_interval = ok.iter().filter_map(|r| r.interval).max().unwrap_or(0);
            let interval = match s.force_interval {
                Some(f) => f,
                None => {
                    Duration::from_secs(u64::from(tracker_interval)).max(s.min_announce_interval)
                }
            }
            .min(MAX_INTERVAL);
            let min_interval = ok
                .iter()
                .filter_map(|r| r.min_interval)
                .max()
                .map(|m| Duration::from_secs(u64::from(m)))
                .unwrap_or(DEFAULT_MIN_INTERVAL)
                .min(interval);
            e.next_announce = Some(now + interval);
            e.min_announce = Some(now + min_interval);
            match event {
                AnnounceEvent::Started => e.start_sent = true,
                AnnounceEvent::Completed => e.complete_sent = true,
                AnnounceEvent::Stopped => e.start_sent = false,
                AnnounceEvent::None => {}
            }
            self.prioritize(idx);
            Some(AppliedAnnounce { success: true })
        } else {
            let err: Option<&TrackerError> = outcome
                .endpoints
                .iter()
                .filter_map(|ep| ep.result.as_ref().err())
                .max_by_key(|e| error_rank(e.kind));
            e.fails = e.fails.saturating_add(1).min(127);
            e.last_error = Some(err.map(|e| e.kind).unwrap_or(TrackerErrorKind::Other));
            e.message = err.map(|e| e.message.clone()).unwrap_or_default();
            if outcome
                .endpoints
                .iter()
                .any(|ep| ep.result.as_ref().is_err_and(|e| e.never_retry))
            {
                e.fail_limit = 1;
            }
            let asked = outcome
                .endpoints
                .iter()
                .filter_map(|ep| ep.result.as_ref().err().and_then(|e| e.retry_in))
                .max()
                .unwrap_or_default();
            e.next_announce = Some(now + retry_delay(e.fails).max(asked).min(MAX_INTERVAL));
            self.deprioritize(idx);
            Some(AppliedAnnounce { success: false })
        }
    }

    /// Trackers that should be scraped now: working trackers whose announce
    /// response lacked swarm counters, at most every [`SCRAPE_INTERVAL`].
    pub fn plan_scrapes(&mut self, now: Instant) -> Vec<PlannedScrape> {
        let mut out = Vec::new();
        for e in self.entries.iter_mut() {
            let lacks_counters = e.endpoints.iter().any(|ep| {
                ep.state == TrackerState::Working && (ep.downloaded.is_none() || ep.seeds.is_none())
            });
            if e.verified
                && lacks_counters
                && !e.scrape_updating
                && !e.scrape_unsupported
                && is_supported_scheme(&e.parsed)
                && e.last_scrape
                    .is_none_or(|t| now.duration_since(t) >= SCRAPE_INTERVAL)
            {
                e.scrape_updating = true;
                e.last_scrape = Some(now);
                out.push(PlannedScrape {
                    id: e.id,
                    url: e.parsed.clone(),
                    trusted: e.trusted,
                });
            }
        }
        out
    }

    pub fn apply_scrape(&mut self, id: TrackerId, result: Result<ScrapeData, TrackerError>) {
        let Some(idx) = self.position(id) else {
            return;
        };
        let e = &mut self.entries[idx];
        e.scrape_updating = false;
        match result {
            Ok(data) => e.scrape = Some(data),
            Err(err) if err.kind == TrackerErrorKind::Skipped => e.scrape_unsupported = true,
            Err(_) => {}
        }
    }

    /// Statuses of all trackers, in announce order.
    pub fn statuses(&self, now: Instant) -> Vec<TrackerStatus> {
        self.entries.iter().map(|e| Self::status(e, now)).collect()
    }

    fn status(e: &TrackerEntry, now: Instant) -> TrackerStatus {
        let next_announce_in = e.next_announce.map(|t| t.saturating_duration_since(now));
        let min_announce_in = e.min_announce.map(|t| t.saturating_duration_since(now));
        let scrape = e.scrape.unwrap_or_default();
        let endpoints: Vec<TrackerEndpointStatus> = e
            .endpoints
            .iter()
            .map(|ep| TrackerEndpointStatus {
                family: ep.family,
                state: ep.state,
                updating: e.updating,
                message: ep.message.clone(),
                peers: ep.peers,
                seeds: ep.seeds.or(scrape.complete),
                leechers: ep.leechers.or(scrape.incomplete),
                downloaded: ep.downloaded.or(scrape.downloaded),
                next_announce_in,
                min_announce_in,
            })
            .collect();

        let n = endpoints.len();
        let count = |s: TrackerState| endpoints.iter().filter(|ep| ep.state == s).count();
        let state = if n == 0 {
            TrackerState::NotContacted
        } else if count(TrackerState::Working) > 0 {
            TrackerState::Working
        } else if count(TrackerState::TrackerError) > 0 {
            TrackerState::TrackerError
        } else if count(TrackerState::Unreachable) == n {
            TrackerState::Unreachable
        } else if count(TrackerState::Unreachable) + count(TrackerState::NotWorking) == n {
            TrackerState::NotWorking
        } else {
            TrackerState::NotContacted
        };
        let message = endpoints
            .iter()
            .filter(|ep| ep.state == state && !ep.message.is_empty())
            .map(|ep| ep.message.clone())
            .next()
            .unwrap_or_default();
        let max =
            |f: fn(&TrackerEndpointStatus) -> Option<u32>| endpoints.iter().filter_map(f).max();

        TrackerStatus {
            url: e.url.clone(),
            tier: e.tier,
            state,
            updating: e.updating,
            message,
            peers: max(|ep| ep.peers),
            seeds: max(|ep| ep.seeds).or(scrape.complete),
            leechers: max(|ep| ep.leechers).or(scrape.incomplete),
            downloaded: max(|ep| ep.downloaded).or(scrape.downloaded),
            next_announce_in,
            min_announce_in,
            last_announce: e.last_announce,
            endpoints,
        }
    }

    /// Swarm totals: the largest seeder and leecher counts any tracker
    /// reported.
    pub fn swarm_counts(&self) -> (Option<u32>, Option<u32>) {
        let now = Instant::now();
        let statuses = self.statuses(now);
        (
            statuses.iter().filter_map(|s| s.seeds).max(),
            statuses.iter().filter_map(|s| s.leechers).max(),
        )
    }

    /// Whether any announce or scrape is in flight.
    pub fn is_updating(&self) -> bool {
        self.entries.iter().any(|e| e.updating || e.scrape_updating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::EndpointOutcome;
    use crate::response::AnnounceResponse;

    fn list(urls: &[(&str, u32)]) -> TrackerList {
        let mut l = TrackerList::new();
        for (u, t) in urls {
            assert!(l.add(u, *t, TrackerOrigin::Torrent, false));
        }
        l
    }

    fn urls(p: &[PlannedAnnounce]) -> Vec<String> {
        p.iter().map(|p| p.url.to_string()).collect()
    }

    fn ok(interval: u32) -> AnnounceOutcome {
        AnnounceOutcome::single(
            AddressFamily::V4,
            Ok(AnnounceResponse {
                interval: Some(interval),
                complete: Some(3),
                incomplete: Some(4),
                peers: vec!["1.2.3.4:5".parse().unwrap()],
                ..Default::default()
            }),
        )
    }

    fn fail(kind: TrackerErrorKind) -> AnnounceOutcome {
        AnnounceOutcome::single(AddressFamily::Any, Err(TrackerError::new(kind, "boom")))
    }

    fn settings(all_trackers: bool, all_tiers: bool) -> AnnounceSettings {
        AnnounceSettings {
            announce_to_all_trackers: all_trackers,
            announce_to_all_tiers: all_tiers,
            ..Default::default()
        }
    }

    fn id_of(l: &TrackerList, url: &str) -> TrackerId {
        l.entries
            .iter()
            .find(|e| e.parsed.as_str() == url)
            .unwrap()
            .id
    }

    const A: &str = "http://a/announce";
    const B: &str = "http://b/announce";
    const C: &str = "http://c/announce";
    const D: &str = "udp://d:1/";

    #[test]
    fn add_keeps_tier_order_and_dedups() {
        let mut l = list(&[(A, 1), (B, 0)]);
        assert!(l.add(C, 0, TrackerOrigin::User, true));
        assert!(!l.add(B, 5, TrackerOrigin::User, true));
        assert!(!l.add("not a url", 0, TrackerOrigin::User, true));
        assert_eq!(
            l.entries(),
            vec![(B.into(), 0), (C.into(), 0), (A.into(), 1)]
        );
        assert_eq!(l.remove(&[B, "http://nope/"]), 1);
        assert_eq!(l.entries(), vec![(C.into(), 0), (A.into(), 1)]);
    }

    #[test]
    fn edit_and_replace() {
        let mut l = list(&[(A, 0), (B, 1)]);
        assert_eq!(
            l.edit("http://x/", C, true),
            Err(TrackerEditError::NotFound)
        );
        assert_eq!(
            l.edit(A, "garbage", true),
            Err(TrackerEditError::InvalidUrl)
        );
        assert_eq!(l.edit(A, B, true), Err(TrackerEditError::AlreadyExists));
        assert_eq!(l.edit(A, A, true), Ok(()));
        assert_eq!(l.edit(A, C, true), Ok(()));
        assert_eq!(l.entries(), vec![(C.into(), 0), (B.into(), 1)]);

        // Replace keeps state for kept URLs.
        let now = Instant::now();
        let s = settings(false, true);
        let planned = l.plan(now, false, &s);
        let b = id_of(&l, B);
        l.apply_announce(b, planned[1].event, &ok(1800), now, SystemTime::now(), &s);
        l.replace(
            vec![
                (B.into(), 0),
                (D.into(), 0),
                (B.into(), 3),
                ("bad".into(), 0),
            ],
            TrackerOrigin::User,
            true,
        );
        assert_eq!(l.entries(), vec![(B.into(), 0), (D.into(), 0)]);
        assert_eq!(l.statuses(now)[0].state, TrackerState::Working);
        assert_eq!(l.statuses(now)[1].state, TrackerState::NotContacted);
    }

    #[test]
    fn one_tracker_per_tier_by_default() {
        let mut l = list(&[(A, 0), (B, 0), (C, 1)]);
        let now = Instant::now();
        let p = l.plan(now, false, &settings(false, true));
        assert_eq!(urls(&p), vec![A, C]);
        assert!(p.iter().all(|p| p.event == AnnounceEvent::Started));
        // Nothing more while in flight.
        assert!(l.plan(now, false, &settings(false, true)).is_empty());
    }

    #[test]
    fn all_trackers_all_tiers() {
        let mut l = list(&[(A, 0), (B, 0), (C, 1)]);
        let p = l.plan(Instant::now(), false, &settings(true, true));
        assert_eq!(urls(&p), vec![A, B, C]);
    }

    #[test]
    fn all_trackers_first_tier_only() {
        let mut l = list(&[(A, 0), (B, 0), (C, 1)]);
        let s = settings(true, false);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        assert_eq!(urls(&p), vec![A, B]);
        // Both fail: the next tier is used.
        for id in [id_of(&l, A), id_of(&l, B)] {
            l.apply_announce(
                id,
                AnnounceEvent::Started,
                &fail(TrackerErrorKind::Other),
                now,
                SystemTime::now(),
                &s,
            );
        }
        let p = l.plan(now, false, &s);
        assert_eq!(urls(&p), vec![C]);
    }

    #[test]
    fn bep12_single_tracker() {
        let mut l = list(&[(A, 0), (B, 0), (C, 1)]);
        let s = settings(false, false);
        let now = Instant::now();
        assert_eq!(urls(&l.plan(now, false, &s)), vec![A]);
        let a = id_of(&l, A);
        l.apply_announce(
            a,
            AnnounceEvent::Started,
            &fail(TrackerErrorKind::Other),
            now,
            SystemTime::now(),
            &s,
        );
        // A moved behind B; B is tried next.
        assert_eq!(l.entries()[0].0, B);
        assert_eq!(urls(&l.plan(now, false, &s)), vec![B]);
        let b = id_of(&l, B);
        l.apply_announce(
            b,
            AnnounceEvent::Started,
            &fail(TrackerErrorKind::Other),
            now,
            SystemTime::now(),
            &s,
        );
        // Whole tier failing: next tier.
        assert_eq!(urls(&l.plan(now, false, &s)), vec![C]);
    }

    #[test]
    fn failover_within_tier_and_promotion() {
        let mut l = list(&[(A, 0), (B, 0)]);
        let s = settings(false, true);
        let t0 = Instant::now();
        assert_eq!(urls(&l.plan(t0, false, &s)), vec![A]);
        let a = id_of(&l, A);
        l.apply_announce(
            a,
            AnnounceEvent::Started,
            &fail(TrackerErrorKind::Other),
            t0,
            SystemTime::now(),
            &s,
        );
        assert_eq!(urls(&l.plan(t0, false, &s)), vec![B]);
        let b = id_of(&l, B);
        l.apply_announce(
            b,
            AnnounceEvent::Started,
            &ok(1800),
            t0,
            SystemTime::now(),
            &s,
        );
        assert_eq!(l.entries()[0].0, B);
        // A's retry time passes, but B works: A stays a backup.
        let t1 = t0 + Duration::from_secs(120);
        assert!(l.plan(t1, false, &s).is_empty());
        // Next wakeup is B's interval, not A's retry.
        assert_eq!(l.next_wakeup(t1, &s), Some(t0 + Duration::from_secs(1800)));
        // At B's interval, B is announced with no event.
        let t2 = t0 + Duration::from_secs(1800);
        let p = l.plan(t2, false, &s);
        assert_eq!(urls(&p), vec![B]);
        assert_eq!(p[0].event, AnnounceEvent::None);
    }

    #[test]
    fn backoff_schedule() {
        assert_eq!(retry_delay(1), Duration::from_secs(17));
        assert_eq!(retry_delay(2), Duration::from_secs(55));
        assert_eq!(retry_delay(3), Duration::from_secs(117));
        assert_eq!(retry_delay(100), Duration::from_secs(3600));

        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        for (i, expected) in [17u64, 55, 117].into_iter().enumerate() {
            let t = now + Duration::from_secs(10_000 * i as u64);
            let p = l.plan(t, false, &s);
            assert_eq!(p.len(), 1, "round {i}");
            l.apply_announce(
                p[0].id,
                p[0].event,
                &fail(TrackerErrorKind::Other),
                t,
                SystemTime::now(),
                &s,
            );
            assert_eq!(
                l.next_wakeup(t, &s),
                Some(t + Duration::from_secs(expected))
            );
            assert!(
                l.plan(t + Duration::from_secs(expected - 1), false, &s)
                    .is_empty()
            );
        }
    }

    #[test]
    fn retry_in_and_never() {
        let mut l = list(&[(A, 0), (B, 1)]);
        let s = settings(false, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        let mut e = TrackerError::tracker_failure("go away");
        e.retry_in = Some(Duration::from_secs(600));
        l.apply_announce(
            p[0].id,
            p[0].event,
            &AnnounceOutcome::single(AddressFamily::Any, Err(e)),
            now,
            SystemTime::now(),
            &s,
        );
        assert!(l.plan(now + Duration::from_secs(599), false, &s).is_empty());
        let gone = TrackerError::other("410").permanent();
        l.apply_announce(
            p[1].id,
            p[1].event,
            &AnnounceOutcome::single(AddressFamily::Any, Err(gone)),
            now,
            SystemTime::now(),
            &s,
        );
        let later = now + Duration::from_secs(100_000);
        assert_eq!(urls(&l.plan(later, false, &s)), vec![A]);
    }

    #[test]
    fn min_interval_and_force() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        let mut r = ok(1800);
        if let Ok(resp) = &mut r.endpoints[0].result {
            resp.min_interval = Some(120);
        }
        l.apply_announce(p[0].id, p[0].event, &r, now, SystemTime::now(), &s);
        let st = &l.statuses(now)[0];
        assert_eq!(st.next_announce_in, Some(Duration::from_secs(1800)));
        assert_eq!(st.min_announce_in, Some(Duration::from_secs(120)));
        assert!(l.plan(now, false, &s).is_empty());
        // Forced reannounce ignores the minimum interval.
        assert!(l.force_reannounce(None));
        let p = l.plan(now, false, &s);
        assert_eq!(urls(&p), vec![A]);
        assert_eq!(p[0].event, AnnounceEvent::None);
        assert!(!l.force_reannounce(Some("http://unknown/")));
    }

    #[test]
    fn forced_backup_tracker_bypasses_tier_selection() {
        let mut l = list(&[(A, 0), (B, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        assert!(l.plan(now, false, &s).is_empty());
        assert!(l.force_reannounce(Some(B)));
        assert_eq!(l.next_wakeup(now, &s), Some(now));
        assert_eq!(urls(&l.plan(now, false, &s)), vec![B]);
    }

    #[test]
    fn hostile_intervals_are_clamped() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        let mut r = ok(u32::MAX);
        if let Ok(resp) = &mut r.endpoints[0].result {
            resp.min_interval = Some(u32::MAX);
        }
        l.apply_announce(p[0].id, p[0].event, &r, now, SystemTime::now(), &s);
        let st = &l.statuses(now)[0];
        assert_eq!(st.next_announce_in, Some(MAX_INTERVAL));
        assert_eq!(st.min_announce_in, Some(MAX_INTERVAL));

        l.force_reannounce(None);
        let p = l.plan(now, false, &s);
        let mut e = TrackerError::tracker_failure("later");
        e.retry_in = Some(Duration::from_secs(u64::MAX / 4));
        l.apply_announce(
            p[0].id,
            p[0].event,
            &AnnounceOutcome::single(AddressFamily::Any, Err(e)),
            now,
            SystemTime::now(),
            &s,
        );
        assert_eq!(l.statuses(now)[0].next_announce_in, Some(MAX_INTERVAL));
    }

    #[test]
    fn interval_floor_and_force_interval() {
        let mut l = list(&[(A, 0)]);
        let now = Instant::now();
        let s = settings(false, true);
        let p = l.plan(now, false, &s);
        l.apply_announce(p[0].id, p[0].event, &ok(10), now, SystemTime::now(), &s);
        assert_eq!(
            l.statuses(now)[0].next_announce_in,
            Some(Duration::from_secs(300))
        );

        let mut l = list(&[(A, 0)]);
        let s = AnnounceSettings {
            force_interval: Some(Duration::from_secs(2)),
            ..settings(false, true)
        };
        let p = l.plan(now, false, &s);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        assert_eq!(
            l.statuses(now)[0].next_announce_in,
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            l.statuses(now)[0].min_announce_in,
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn events_started_completed_stopped() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        l.reset_for_start(false);
        let p = l.plan(now, false, &s);
        assert_eq!(p[0].event, AnnounceEvent::Started);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        // Becoming a seed: completed is sent despite the interval.
        l.on_completed();
        let p = l.plan(now, true, &s);
        assert_eq!(p[0].event, AnnounceEvent::Completed);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        let later = now + Duration::from_secs(1800);
        let p = l.plan(later, true, &s);
        assert_eq!(p[0].event, AnnounceEvent::None);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), later, SystemTime::now(), &s);

        let stop = l.plan_stop();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].event, AnnounceEvent::Stopped);
        l.reset_after_stop();
        assert_eq!(l.statuses(now)[0].state, TrackerState::NotContacted);
        assert_eq!(l.statuses(now)[0].next_announce_in, None);
        assert!(l.plan_stop().is_empty());

        // Starting as a seed never sends completed.
        l.reset_for_start(true);
        let p = l.plan(now, true, &s);
        assert_eq!(p[0].event, AnnounceEvent::Started);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        let p = l.plan(later, true, &s);
        assert_eq!(p[0].event, AnnounceEvent::None);
    }

    #[test]
    fn failed_started_is_retried_as_started() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        l.apply_announce(
            p[0].id,
            p[0].event,
            &fail(TrackerErrorKind::Other),
            now,
            SystemTime::now(),
            &s,
        );
        assert!(l.plan_stop().is_empty());
        let p = l.plan(now + Duration::from_secs(60), false, &s);
        assert_eq!(p[0].event, AnnounceEvent::Started);
    }

    #[test]
    fn state_mapping_and_aggregation() {
        let mut l = list(&[(A, 0), (B, 1), (C, 2), (D, 3)]);
        let s = settings(true, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        assert_eq!(p.len(), 4);
        let st = l.statuses(now);
        assert!(
            st.iter()
                .all(|s| s.updating && s.state == TrackerState::NotContacted)
        );

        l.apply_announce(
            id_of(&l, A),
            AnnounceEvent::Started,
            &fail(TrackerErrorKind::TrackerFailure),
            now,
            SystemTime::now(),
            &s,
        );
        l.apply_announce(
            id_of(&l, B),
            AnnounceEvent::Started,
            &fail(TrackerErrorKind::Other),
            now,
            SystemTime::now(),
            &s,
        );
        l.apply_announce(
            id_of(&l, C),
            AnnounceEvent::Started,
            &fail(TrackerErrorKind::Skipped),
            now,
            SystemTime::now(),
            &s,
        );
        // Dual-stack UDP: v4 works, v6 fails -> Working overall.
        let mixed = AnnounceOutcome {
            endpoints: vec![
                EndpointOutcome {
                    family: AddressFamily::V4,
                    result: Ok(AnnounceResponse {
                        interval: Some(1800),
                        complete: Some(7),
                        incomplete: Some(1),
                        warning_message: Some("careful".into()),
                        ..Default::default()
                    }),
                },
                EndpointOutcome {
                    family: AddressFamily::V6,
                    result: Err(TrackerError::other("network unreachable")),
                },
            ],
        };
        l.apply_announce(
            id_of(&l, D),
            AnnounceEvent::Started,
            &mixed,
            now,
            SystemTime::now(),
            &s,
        );

        let st = l.statuses(now);
        let by_url = |u: &str| st.iter().find(|s| s.url == u).unwrap().clone();
        assert_eq!(by_url(A).state, TrackerState::TrackerError);
        assert_eq!(by_url(A).message, "boom");
        assert_eq!(by_url(B).state, TrackerState::NotWorking);
        assert_eq!(by_url(C).state, TrackerState::Unreachable);
        let d = by_url(D);
        assert_eq!(d.state, TrackerState::Working);
        assert_eq!(d.message, "careful");
        assert_eq!(d.seeds, Some(7));
        assert_eq!(d.leechers, Some(1));
        assert_eq!(d.peers, Some(0));
        assert_eq!(d.endpoints.len(), 2);
        assert_eq!(d.endpoints[1].state, TrackerState::NotWorking);
        assert!(d.last_announce.is_some());
        assert_eq!(l.swarm_counts(), (Some(7), Some(1)));
    }

    #[test]
    fn scrape_planning() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        assert!(l.plan_scrapes(now).is_empty());
        let p = l.plan(now, false, &s);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        let sc = l.plan_scrapes(now);
        assert_eq!(sc.len(), 1);
        assert!(l.plan_scrapes(now).is_empty());
        l.apply_scrape(
            sc[0].id,
            Ok(ScrapeData {
                complete: Some(9),
                incomplete: Some(2),
                downloaded: Some(40),
            }),
        );
        let st = &l.statuses(now)[0];
        assert_eq!(st.downloaded, Some(40));
        // Announce values win over scrape values.
        assert_eq!(st.seeds, Some(3));
        assert!(l.plan_scrapes(now + Duration::from_secs(60)).is_empty());
        assert_eq!(l.plan_scrapes(now + SCRAPE_INTERVAL).len(), 1);
        l.apply_scrape(sc[0].id, Err(TrackerError::skipped("scrape not supported")));
        assert!(l.plan_scrapes(now + SCRAPE_INTERVAL * 3).is_empty());
    }

    #[test]
    fn removed_while_in_flight() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        let p = l.plan(now, false, &s);
        l.remove(&[A]);
        assert!(
            l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s)
                .is_none()
        );
    }

    #[test]
    fn remove_origin() {
        let mut l = list(&[(A, 0)]);
        l.add(B, 1, TrackerOrigin::Session, true);
        assert_eq!(l.remove_origin(TrackerOrigin::Session), 1);
        assert_eq!(l.entries(), vec![(A.into(), 0)]);
    }

    #[test]
    fn caps_tracker_count() {
        let mut l = TrackerList::new();
        for i in 0..(MAX_TRACKERS + 10) {
            l.add(
                &format!("http://t{i}.example/announce"),
                0,
                TrackerOrigin::Torrent,
                false,
            );
        }
        assert_eq!(l.len(), MAX_TRACKERS);
    }

    #[test]
    fn complete_when_first_checked_sends_no_completed() {
        let mut l = list(&[(A, 0)]);
        let s = settings(false, true);
        let now = Instant::now();
        l.reset_for_start(false);
        let p = l.plan(now, false, &s);
        l.apply_announce(p[0].id, p[0].event, &ok(1800), now, SystemTime::now(), &s);
        l.mark_complete_sent();
        assert!(l.plan(now, true, &s).is_empty());
        let later = now + Duration::from_secs(1800);
        assert_eq!(l.plan(later, true, &s)[0].event, AnnounceEvent::None);
    }
}
