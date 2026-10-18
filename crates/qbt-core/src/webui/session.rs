//! WebUI sessions (qBittorrent `WebSession`) and the failed-login ban list.
//!
//! * Session IDs are 24 bytes from the OS CSPRNG, base64-encoded (192 bits).
//! * Sessions are stored by the SHA-256 of their ID, so looking up an ID
//!   leaks no timing information about stored IDs.
//! * Expiry is idle-based; API-key sessions never expire.
//! * The store is bounded: expired sessions are swept whenever a session is
//!   created and the least recently used one is evicted when full.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_lc_rs::digest;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use parking_lot::Mutex;

use super::api::SessionData;
use super::password::random_bytes;

/// Maximum number of concurrent sessions.
pub const MAX_SESSIONS: usize = 512;
/// Maximum number of tracked clients with failed logins.
pub const MAX_BAN_ENTRIES: usize = 16_384;

type SessionKey = [u8; 32];

fn key_of(sid: &str) -> SessionKey {
    let d = digest::digest(&digest::SHA256, sid.as_bytes());
    let mut k = [0u8; 32];
    k.copy_from_slice(d.as_ref());
    k
}

struct Times {
    last_access: Instant,
    /// When the cookie must be re-sent (`None` = right away).
    cookie_refresh_at: Option<Instant>,
    cookie_generation: u64,
}

pub struct WebSession {
    id: String,
    api_key: bool,
    times: Mutex<Times>,
    /// Per-session controller state (sync snapshots, search jobs, metadata
    /// caches). Async mutex: handlers may hold it across facade calls.
    pub data: tokio::sync::Mutex<SessionData>,
}

impl WebSession {
    fn new(id: String, api_key: bool, now: Instant) -> Self {
        Self {
            id,
            api_key,
            times: Mutex::new(Times { last_access: now, cookie_refresh_at: None, cookie_generation: 0 }),
            data: tokio::sync::Mutex::new(SessionData::default()),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn is_api_key(&self) -> bool {
        self.api_key
    }

    fn has_expired(&self, timeout: Option<Duration>, now: Instant) -> bool {
        match timeout {
            Some(t) if !self.api_key => now.saturating_duration_since(self.times.lock().last_access) > t,
            _ => false,
        }
    }

    fn touch(&self, now: Instant) {
        self.times.lock().last_access = now;
    }

    fn last_access(&self) -> Instant {
        self.times.lock().last_access
    }

    /// Whether the cookie should be (re-)sent: its refresh time passed or the
    /// configuration changed since it was issued.
    pub fn should_refresh_cookie(&self, now: Instant, generation: u64) -> bool {
        let t = self.times.lock();
        t.cookie_generation != generation || t.cookie_refresh_at.is_none_or(|at| now >= at)
    }

    /// Records that the cookie was sent; refresh after min(timeout / 2, 1 day).
    pub fn cookie_sent(&self, now: Instant, cookie_lifetime: Duration, generation: u64) {
        let refresh = (cookie_lifetime / 2).min(Duration::from_secs(86_400));
        let mut t = self.times.lock();
        t.cookie_refresh_at = Some(now + refresh);
        t.cookie_generation = generation;
    }
}

#[derive(Default)]
pub struct SessionStore {
    sessions: HashMap<SessionKey, Arc<WebSession>>,
}

impl SessionStore {
    /// Finds a live session and refreshes its idle timer; drops it if expired.
    pub fn lookup(&mut self, sid: &str, timeout: Option<Duration>, now: Instant) -> Option<Arc<WebSession>> {
        if sid.is_empty() || sid.len() > 128 {
            return None;
        }
        let key = key_of(sid);
        let s = self.sessions.get(&key)?.clone();
        if s.has_expired(timeout, now) {
            self.sessions.remove(&key);
            return None;
        }
        s.touch(now);
        Some(s)
    }

    /// Creates a cookie session with a fresh random ID.
    pub fn create(&mut self, timeout: Option<Duration>, now: Instant) -> Arc<WebSession> {
        let sid = loop {
            let mut raw = [0u8; 24];
            random_bytes(&mut raw);
            let sid = B64.encode(raw);
            if !self.sessions.contains_key(&key_of(&sid)) {
                break sid;
            }
        };
        self.insert(sid, false, timeout, now)
    }

    /// API-key session: its ID is the key itself and it never expires.
    pub fn api_key_session(&mut self, key: &str, now: Instant) -> Arc<WebSession> {
        if let Some(s) = self.sessions.get(&key_of(key)) {
            s.touch(now);
            return s.clone();
        }
        self.insert(key.to_owned(), true, None, now)
    }

    fn insert(&mut self, sid: String, api_key: bool, timeout: Option<Duration>, now: Instant) -> Arc<WebSession> {
        self.sessions.retain(|_, s| !s.has_expired(timeout, now));
        while self.sessions.len() >= MAX_SESSIONS {
            let victim = self
                .sessions
                .iter()
                .filter(|(_, s)| !s.api_key)
                .min_by_key(|(_, s)| s.last_access())
                .or_else(|| self.sessions.iter().min_by_key(|(_, s)| s.last_access()))
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.sessions.remove(&k);
                }
                None => break,
            }
        }
        let s = Arc::new(WebSession::new(sid, api_key, now));
        self.sessions.insert(key_of(&s.id), s.clone());
        s
    }

    pub fn remove(&mut self, sid: &str) {
        self.sessions.remove(&key_of(sid));
    }

    /// Drops API-key sessions whose key is no longer the configured one.
    pub fn retain_api_key(&mut self, current: Option<&str>) {
        let keep = current.map(key_of);
        self.sessions.retain(|k, s| !s.api_key || Some(*k) == keep);
    }

    /// Drops every cookie (login / bypass) session; API-key sessions stay.
    pub fn clear_cookie_sessions(&mut self) {
        self.sessions.retain(|_, s| s.api_key);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn clear(&mut self) {
        self.sessions.clear();
    }
}

#[derive(Clone, Copy, Debug)]
struct FailedLogin {
    count: u32,
    /// Verifications in progress; they count against the limit so that
    /// concurrent attempts cannot all slip past the ban check.
    pending: u32,
    banned_until: Option<Instant>,
    last: Instant,
}

/// Failed authentication attempts per client address (memory only).
#[derive(Default)]
pub struct BanList {
    entries: HashMap<IpAddr, FailedLogin>,
}

/// Key of a client in the ban list: IPv6 clients are tracked per /64, the
/// smallest block an end site is normally given, so an attacker cannot evade
/// the ban by rotating through the addresses of its own prefix.
fn ban_key(client: IpAddr) -> IpAddr {
    match client.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        v4 => v4,
    }
}

impl BanList {
    /// Whether `client` is banned; an expired ban is erased with its counter.
    pub fn is_banned(&mut self, client: IpAddr, now: Instant) -> bool {
        let client = ban_key(client);
        let Some(e) = self.entries.get(&client) else { return false };
        match e.banned_until {
            Some(until) if now < until => true,
            Some(_) => {
                self.entries.remove(&client);
                false
            }
            None => false,
        }
    }

    /// Starts a credential check: refused (as banned) when the client is
    /// banned or when its failures plus the checks already in progress reach
    /// `max_fail`. Every `true` must be paired with [`Self::end_attempt`].
    pub fn begin_attempt(&mut self, client: IpAddr, max_fail: i64, now: Instant) -> bool {
        if self.is_banned(client, now) {
            return false;
        }
        if max_fail <= 0 {
            return true;
        }
        let key = ban_key(client);
        self.make_room(key, now);
        let e = self.entries.entry(key).or_insert(FailedLogin { count: 0, pending: 0, banned_until: None, last: now });
        if i64::from(e.count) + i64::from(e.pending) >= max_fail {
            return false;
        }
        e.pending += 1;
        e.last = now;
        true
    }

    /// Ends a check started by [`Self::begin_attempt`]; record its outcome
    /// with [`Self::record_failure`] or [`Self::clear`].
    pub fn end_attempt(&mut self, client: IpAddr) {
        let key = ban_key(client);
        if let Some(e) = self.entries.get_mut(&key) {
            e.pending = e.pending.saturating_sub(1);
            if e.pending == 0 && e.count == 0 && e.banned_until.is_none() {
                self.entries.remove(&key);
            }
        }
    }

    fn make_room(&mut self, key: IpAddr, now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_BAN_ENTRIES {
            self.entries.retain(|_, e| e.pending > 0 || e.banned_until.is_some_and(|u| now < u));
            if self.entries.len() >= MAX_BAN_ENTRIES
                && let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.last).map(|(k, _)| *k)
            {
                self.entries.remove(&oldest);
            }
        }
    }

    pub fn failed_attempts(&self, client: IpAddr) -> u32 {
        let client = ban_key(client);
        self.entries.get(&client).map(|e| e.count).unwrap_or(0)
    }

    /// Counts a failure; bans the client once `max_fail` is reached.
    pub fn record_failure(&mut self, client: IpAddr, max_fail: i64, ban: Duration, now: Instant) -> u32 {
        let client = ban_key(client);
        if max_fail <= 0 {
            return 0;
        }
        self.make_room(client, now);
        let e = self.entries.entry(client).or_insert(FailedLogin { count: 0, pending: 0, banned_until: None, last: now });
        e.count = e.count.saturating_add(1);
        e.last = now;
        if i64::from(e.count) >= max_fail {
            e.banned_until = Some(now + ban);
        }
        e.count
    }

    /// Successful login: forgets the failures (checks still in progress
    /// keep their slot).
    pub fn clear(&mut self, client: IpAddr) {
        let client = ban_key(client);
        if let Some(e) = self.entries.get_mut(&client) {
            if e.pending == 0 {
                self.entries.remove(&client);
            } else {
                e.count = 0;
                e.banned_until = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle() {
        let mut st = SessionStore::default();
        let t0 = Instant::now();
        let timeout = Some(Duration::from_secs(60));
        let s = st.create(timeout, t0);
        assert_eq!(s.id().len(), 32);
        assert!(st.lookup(s.id(), timeout, t0 + Duration::from_secs(30)).is_some());
        // Idle timer was refreshed at +30s.
        assert!(st.lookup(s.id(), timeout, t0 + Duration::from_secs(80)).is_some());
        assert!(st.lookup(s.id(), timeout, t0 + Duration::from_secs(200)).is_none());
        assert_eq!(st.len(), 0);
        // Unknown / forged IDs never create sessions.
        assert!(st.lookup("forged", timeout, t0).is_none());
        assert!(st.lookup("", timeout, t0).is_none());
        assert_eq!(st.len(), 0);
        // No timeout: never expires.
        let s = st.create(None, t0);
        assert!(st.lookup(s.id(), None, t0 + Duration::from_secs(1_000_000)).is_some());
        st.remove(s.id());
        assert!(st.lookup(s.id(), None, t0).is_none());
    }

    #[test]
    fn session_ids_are_unique_and_random() {
        let mut st = SessionStore::default();
        let t0 = Instant::now();
        let a = st.create(None, t0);
        let b = st.create(None, t0);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn bounded_store_evicts_lru() {
        let mut st = SessionStore::default();
        let t0 = Instant::now();
        let first = st.create(None, t0);
        let mut last = None;
        for i in 1..=MAX_SESSIONS {
            last = Some(st.create(None, t0 + Duration::from_millis(i as u64)));
        }
        assert_eq!(st.len(), MAX_SESSIONS);
        assert!(st.lookup(first.id(), None, t0).is_none());
        assert!(st.lookup(last.unwrap().id(), None, t0).is_some());
    }

    #[test]
    fn api_key_sessions() {
        let mut st = SessionStore::default();
        let t0 = Instant::now();
        let a = st.api_key_session("qbt_key1", t0);
        let b = st.api_key_session("qbt_key1", t0);
        assert!(Arc::ptr_eq(&a, &b));
        assert!(a.is_api_key());
        // Never expires even with a timeout.
        assert!(st.lookup("qbt_key1", Some(Duration::from_secs(1)), t0 + Duration::from_secs(100)).is_some());
        st.retain_api_key(Some("qbt_key2"));
        assert!(st.lookup("qbt_key1", None, t0).is_none());
    }

    #[test]
    fn cookie_refresh() {
        let t0 = Instant::now();
        let s = WebSession::new("x".into(), false, t0);
        assert!(s.should_refresh_cookie(t0, 0));
        s.cookie_sent(t0, Duration::from_secs(3600), 0);
        assert!(!s.should_refresh_cookie(t0 + Duration::from_secs(10), 0));
        assert!(s.should_refresh_cookie(t0 + Duration::from_secs(1800), 0));
        // Preference change -> re-send.
        assert!(s.should_refresh_cookie(t0, 1));
    }

    #[test]
    fn bans() {
        let mut b = BanList::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let t0 = Instant::now();
        let ban = Duration::from_secs(60);
        for i in 1..5 {
            assert_eq!(b.record_failure(ip, 5, ban, t0), i);
            assert!(!b.is_banned(ip, t0));
        }
        b.record_failure(ip, 5, ban, t0);
        assert!(b.is_banned(ip, t0 + Duration::from_secs(59)));
        assert!(!b.is_banned("1.2.3.5".parse().unwrap(), t0));
        // Expired ban erases the record.
        assert!(!b.is_banned(ip, t0 + Duration::from_secs(61)));
        assert_eq!(b.failed_attempts(ip), 0);
        // max_fail 0 = never ban
        for _ in 0..10 {
            b.record_failure(ip, 0, ban, t0);
        }
        assert!(!b.is_banned(ip, t0));
        b.record_failure(ip, 2, ban, t0);
        b.clear(ip);
        assert_eq!(b.failed_attempts(ip), 0);
    }

    #[test]
    fn concurrent_attempts_count_against_the_limit() {
        let mut b = BanList::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let t0 = Instant::now();
        let ban = Duration::from_secs(60);
        // With 3 allowed failures, only 3 checks may run at once.
        assert!(b.begin_attempt(ip, 3, t0));
        assert!(b.begin_attempt(ip, 3, t0));
        assert!(b.begin_attempt(ip, 3, t0));
        assert!(!b.begin_attempt(ip, 3, t0));
        // Another client is unaffected.
        assert!(b.begin_attempt("1.2.3.5".parse().unwrap(), 3, t0));
        // One failed check finishes: still at the limit (2 pending + 1 failure).
        b.end_attempt(ip);
        b.record_failure(ip, 3, ban, t0);
        assert!(!b.begin_attempt(ip, 3, t0));
        // A check that ends without an outcome (dropped request) frees its slot.
        b.end_attempt(ip);
        assert!(b.begin_attempt(ip, 3, t0));
        b.end_attempt(ip);
        b.end_attempt(ip);
        b.record_failure(ip, 3, ban, t0);
        b.record_failure(ip, 3, ban, t0);
        assert!(b.is_banned(ip, t0));
        assert!(!b.begin_attempt(ip, 3, t0));
        // Idle clients leave no entry behind.
        let other: IpAddr = "9.9.9.9".parse().unwrap();
        assert!(b.begin_attempt(other, 3, t0));
        b.end_attempt(other);
        assert!(!b.entries.contains_key(&other));
        // max_fail 0 = never ban, never limited.
        for _ in 0..10 {
            assert!(b.begin_attempt(other, 0, t0));
        }
    }

    #[test]
    fn ipv6_bans_cover_the_whole_64() {
        let mut b = BanList::default();
        let t0 = Instant::now();
        let ban = Duration::from_secs(60);
        // An attacker rotating addresses inside its /64 is still counted once.
        for i in 0..5u16 {
            let ip: IpAddr = format!("2001:db8:1:2::{:x}", i + 1).parse().unwrap();
            b.record_failure(ip, 5, ban, t0);
        }
        assert!(b.is_banned("2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap(), t0));
        assert!(!b.is_banned("2001:db8:1:3::1".parse().unwrap(), t0));
        // IPv4 (also mapped) stays per address.
        b.record_failure("::ffff:10.0.0.1".parse().unwrap(), 1, ban, t0);
        assert!(b.is_banned("10.0.0.1".parse().unwrap(), t0));
        assert!(!b.is_banned("10.0.0.2".parse().unwrap(), t0));
    }

    #[test]
    fn ban_list_is_bounded() {
        let mut b = BanList::default();
        let t0 = Instant::now();
        for i in 0..(MAX_BAN_ENTRIES as u32 + 100) {
            let ip = IpAddr::from(i.to_be_bytes());
            b.record_failure(ip, 5, Duration::from_secs(60), t0);
        }
        assert!(b.entries.len() <= MAX_BAN_ENTRIES);
    }
}
