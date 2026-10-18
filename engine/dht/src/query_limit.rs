// Per-source limit on the incoming queries we answer.
//
// Every DHT query is answered with a reply several times its size (nodes of
// both families, up to 100 stored peers). UDP source addresses can be
// spoofed, so without a limit anyone can point a stream of queries "from" a
// victim at us and have us flood the victim (reflection). Queries from one
// address (IPv6: one /64) are answered at most `MAX_QUERIES_PER_SOURCE` times
// per second; the table is reset every second and holds at most
// `MAX_SOURCES` addresses (queries from further addresses in that second are
// dropped, which bounds the total reply rate as well).

use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

pub(crate) const MAX_QUERIES_PER_SOURCE: u16 = 25;
pub(crate) const MAX_SOURCES: usize = 4096;
const WINDOW: Duration = Duration::from_secs(1);

pub(crate) struct QueryLimiter {
    window_start: Instant,
    counts: HashMap<IpAddr, u16>,
}

impl QueryLimiter {
    pub fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            counts: HashMap::new(),
        }
    }

    /// Whether a query from `ip` may be answered (and counts it).
    pub fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        // Loopback can't be spoofed from outside (local test swarms share it).
        if ip.to_canonical().is_loopback() {
            return true;
        }
        if now.saturating_duration_since(self.window_start) >= WINDOW {
            self.window_start = now;
            self.counts.clear();
            if self.counts.capacity() > MAX_SOURCES {
                self.counts.shrink_to(64);
            }
        }
        let key = match ip.to_canonical() {
            IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & (u128::MAX << 64)).into()),
            v4 => v4,
        };
        let len = self.counts.len();
        match self.counts.get_mut(&key) {
            Some(n) if *n >= MAX_QUERIES_PER_SOURCE => false,
            Some(n) => {
                *n += 1;
                true
            }
            None if len >= MAX_SOURCES => false,
            None => {
                self.counts.insert(key, 1);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_each_source_per_second() {
        let t0 = Instant::now();
        let mut l = QueryLimiter::new(t0);
        let a: IpAddr = "203.0.113.5".parse().unwrap();
        for _ in 0..MAX_QUERIES_PER_SOURCE {
            assert!(l.allow(a, t0));
        }
        assert!(!l.allow(a, t0));
        // IPv4-mapped form is the same source.
        assert!(!l.allow("::ffff:203.0.113.5".parse().unwrap(), t0));
        // Other sources are unaffected.
        assert!(l.allow("203.0.113.6".parse().unwrap(), t0));
        // Loopback is not limited.
        for _ in 0..100 {
            assert!(l.allow("127.0.0.1".parse().unwrap(), t0));
        }
        // A new second starts afresh.
        assert!(l.allow(a, t0 + WINDOW));
    }

    #[test]
    fn ipv6_is_limited_per_64() {
        let t0 = Instant::now();
        let mut l = QueryLimiter::new(t0);
        for i in 0..MAX_QUERIES_PER_SOURCE {
            let ip: IpAddr = format!("2001:db8:1:2::{:x}", i + 1).parse().unwrap();
            assert!(l.allow(ip, t0));
        }
        assert!(!l.allow("2001:db8:1:2:ffff::1".parse().unwrap(), t0));
        assert!(l.allow("2001:db8:1:3::1".parse().unwrap(), t0));
    }

    #[test]
    fn table_is_bounded() {
        let t0 = Instant::now();
        let mut l = QueryLimiter::new(t0);
        for i in 0..MAX_SOURCES as u32 {
            assert!(l.allow(IpAddr::from(i.to_be_bytes()), t0));
        }
        assert!(!l.allow(IpAddr::from(u32::MAX.to_be_bytes()), t0));
        assert!(l.allow(IpAddr::from(u32::MAX.to_be_bytes()), t0 + WINDOW));
    }
}
