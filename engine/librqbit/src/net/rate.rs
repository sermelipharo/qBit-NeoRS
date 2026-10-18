//! Session-wide pacing of outgoing connection attempts (libtorrent
//! `connection_speed`).

use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use parking_lot::Mutex;
use tokio::time::Instant;

pub(crate) struct ConnectRateLimiter {
    per_second: AtomicU32,
    next: Mutex<Option<Instant>>,
}

impl ConnectRateLimiter {
    pub fn new(per_second: u32) -> Self {
        Self {
            per_second: AtomicU32::new(per_second),
            next: Mutex::new(None),
        }
    }

    pub fn set_rate(&self, per_second: u32) {
        self.per_second.store(per_second, Ordering::Relaxed);
        *self.next.lock() = None;
    }

    /// The delay before the next attempt may start, reserving that slot.
    fn reserve(&self, now: Instant) -> Duration {
        let rate = self.per_second.load(Ordering::Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        let interval = Duration::from_secs(1) / rate;
        let mut next = self.next.lock();
        let slot = next.map_or(now, |n| n.max(now));
        *next = Some(slot + interval);
        slot - now
    }

    /// Waits for a connection attempt slot.
    pub async fn acquire(&self) {
        let wait = self.reserve(Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_attempts() {
        let l = ConnectRateLimiter::new(0);
        let now = Instant::now();
        for _ in 0..100 {
            assert_eq!(l.reserve(now), Duration::ZERO);
        }
        l.set_rate(10);
        assert_eq!(l.reserve(now), Duration::ZERO);
        assert_eq!(l.reserve(now), Duration::from_millis(100));
        assert_eq!(l.reserve(now), Duration::from_millis(200));
        // Time passing frees slots.
        assert_eq!(l.reserve(now + Duration::from_secs(5)), Duration::ZERO);
        l.set_rate(0);
        assert_eq!(l.reserve(now), Duration::ZERO);
    }
}
