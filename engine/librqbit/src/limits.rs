//! Byte rate limits: one token bucket per direction.
//!
//! The bucket holds at most a fifth of a second of tokens, starts empty when
//! a limit is set, and changes its rate in place: a new limit applies to the
//! next byte, waiters re-plan when the rate changes and are released at once
//! when the limit is removed. Callers reserve bytes first come, first served:
//! a reservation larger than the available tokens puts the bucket into debt,
//! and the caller sleeps until the refill has paid for it (so a block bigger
//! than the burst, or a limit below one block per second, just waits longer).
//! A reservation dropped before it completes gives its bytes back.

use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitsConfig {
    pub upload_bps: Option<NonZeroU32>,
    pub download_bps: Option<NonZeroU32>,
}

/// The burst a bucket can save up while idle: 200 ms of the rate, so that a
/// 1 s window never sees more than 1.2x the limit.
fn burst(rate: u32) -> f64 {
    (f64::from(rate) / 5.0).max(1.0)
}

struct BucketState {
    // Bytes/s; 0 = unlimited.
    rate: u32,
    // Available bytes; negative while callers wait for reserved bytes.
    tokens: f64,
    // Bytes credited by the refill since the bucket was created. A waiter is
    // done once this reaches the position it got when it reserved.
    credited: f64,
    last: Instant,
}

impl BucketState {
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        if self.rate == 0 {
            return;
        }
        let add = elapsed * f64::from(self.rate);
        self.credited += add;
        self.tokens = (self.tokens + add).min(burst(self.rate));
    }
}

struct Bucket {
    // Mirror of `state.rate` for the unlimited fast path.
    rate: AtomicU32,
    state: Mutex<BucketState>,
    // Rate changes and returned reservations: waiters re-plan.
    changed: Notify,
}

/// Gives the bytes of an abandoned reservation back to the bucket.
struct Reservation<'a> {
    bucket: &'a Bucket,
    bytes: f64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        {
            let mut s = self.bucket.state.lock();
            if s.rate != 0 {
                s.refill(Instant::now());
                s.credited += self.bytes;
                s.tokens = (s.tokens + self.bytes).min(burst(s.rate));
            }
        }
        self.bucket.changed.notify_waiters();
    }
}

impl Bucket {
    fn new(bps: Option<NonZeroU32>) -> Self {
        let rate = bps.map_or(0, NonZeroU32::get);
        Self {
            rate: AtomicU32::new(rate),
            state: Mutex::new(BucketState {
                rate,
                tokens: 0.0,
                credited: 0.0,
                last: Instant::now(),
            }),
            changed: Notify::new(),
        }
    }

    fn get(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.rate.load(Ordering::Relaxed))
    }

    fn set(&self, bps: Option<NonZeroU32>) {
        let rate = bps.map_or(0, NonZeroU32::get);
        {
            let mut s = self.state.lock();
            let now = Instant::now();
            s.refill(now);
            if s.rate == 0 || rate == 0 {
                // A new limit starts with an empty bucket; removing the limit
                // forgives the waiters' debt.
                s.tokens = 0.0;
            } else {
                s.tokens = s.tokens.min(burst(rate));
            }
            s.rate = rate;
            self.rate.store(rate, Ordering::Relaxed);
        }
        self.changed.notify_waiters();
    }

    async fn acquire(&self, bytes: u32) {
        if bytes == 0 || self.rate.load(Ordering::Relaxed) == 0 {
            return;
        }
        let target = {
            let mut s = self.state.lock();
            if s.rate == 0 {
                return;
            }
            s.refill(Instant::now());
            s.tokens -= f64::from(bytes);
            if s.tokens >= 0.0 {
                return;
            }
            s.credited - s.tokens
        };
        let reservation = Reservation {
            bucket: self,
            bytes: f64::from(bytes),
        };
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let deadline = {
                let mut s = self.state.lock();
                if s.rate == 0 {
                    break;
                }
                let now = Instant::now();
                s.refill(now);
                if s.credited >= target {
                    break;
                }
                now + Duration::from_secs_f64((target - s.credited) / f64::from(s.rate))
            };
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {}
                _ = notified => {}
            }
        }
        std::mem::forget(reservation);
    }
}

pub struct Limits {
    down: Bucket,
    up: Bucket,
}

impl Limits {
    pub fn new(config: LimitsConfig) -> Self {
        Self {
            down: Bucket::new(config.download_bps),
            up: Bucket::new(config.upload_bps),
        }
    }

    pub async fn prepare_for_upload(&self, len: NonZeroU32) -> crate::Result<()> {
        self.up.acquire(len.get()).await;
        Ok(())
    }

    pub async fn prepare_for_download(&self, len: NonZeroU32) -> crate::Result<()> {
        self.down.acquire(len.get()).await;
        Ok(())
    }

    pub fn set_upload_bps(&self, bps: Option<NonZeroU32>) {
        self.up.set(bps);
    }

    pub fn set_download_bps(&self, bps: Option<NonZeroU32>) {
        self.down.set(bps);
    }

    pub fn get_upload_bps(&self) -> Option<NonZeroU32> {
        self.up.get()
    }

    pub fn get_download_bps(&self) -> Option<NonZeroU32> {
        self.down.get()
    }

    pub fn get_config(&self) -> LimitsConfig {
        LimitsConfig {
            upload_bps: self.get_upload_bps(),
            download_bps: self.get_download_bps(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    const BLOCK: NonZeroU32 = NonZeroU32::new(16384).unwrap();

    fn limits(down: u32) -> Arc<Limits> {
        Arc::new(Limits::new(LimitsConfig {
            upload_bps: None,
            download_bps: NonZeroU32::new(down),
        }))
    }

    /// `n` tasks downloading blocks through the limiter as fast as it lets
    /// them; returns the shared byte counter.
    fn consumers(limits: &Arc<Limits>, n: usize) -> Arc<AtomicU64> {
        let counter = Arc::new(AtomicU64::new(0));
        for _ in 0..n {
            let (l, c) = (limits.clone(), counter.clone());
            tokio::spawn(async move {
                loop {
                    l.prepare_for_download(BLOCK).await.unwrap();
                    c.fetch_add(u64::from(BLOCK.get()), Ordering::Relaxed);
                    tokio::task::yield_now().await;
                }
            });
        }
        counter
    }

    /// Bytes counted over each of the next `secs` seconds.
    async fn per_second(counter: &AtomicU64, secs: usize) -> Vec<u64> {
        let mut out = Vec::new();
        let mut prev = counter.load(Ordering::Relaxed);
        for _ in 0..secs {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = counter.load(Ordering::Relaxed);
            out.push(now - prev);
            prev = now;
        }
        out
    }

    fn assert_within(what: &str, seconds: &[u64], limit: u64) {
        for (i, &b) in seconds.iter().enumerate() {
            assert!(
                b * 4 <= limit * 5 && b * 10 >= limit * 8,
                "{what}: second {i}: {b} B vs limit {limit} B/s ({seconds:?})"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn many_waiters_share_the_limit_from_the_first_second() {
        let l = limits(800 * 1024);
        let counter = consumers(&l, 60);
        let s = per_second(&counter, 5).await;
        assert_within("800 KiB/s, 60 peers", &s, 800 * 1024);
    }

    #[tokio::test(start_paused = true)]
    async fn limit_changes_apply_at_once() {
        let l = Arc::new(Limits::new(LimitsConfig::default()));
        let counter = consumers(&l, 20);
        // Unlimited: no waiting at all.
        tokio::task::yield_now().await;
        let unlimited = counter.load(Ordering::Relaxed);
        assert!(unlimited > 0);

        // Unlimited -> limited: the new limit starts with an empty bucket.
        l.set_download_bps(NonZeroU32::new(500_000));
        tokio::task::yield_now().await;
        let s = per_second(&counter, 3).await;
        assert_within("500 KB/s", &s, 500_000);

        // Lowering: in effect from the next second.
        l.set_download_bps(NonZeroU32::new(100_000));
        let s = per_second(&counter, 4).await;
        assert_within("100 KB/s", &s[1..], 100_000);
        assert!(s[0] * 4 <= 100_000 * 5 + 20 * 16384, "{s:?}");

        // Raising: the waiters re-plan instead of finishing the old schedule.
        l.set_download_bps(NonZeroU32::new(1_000_000));
        let s = per_second(&counter, 3).await;
        assert_within("1 MB/s", &s, 1_000_000);

        // Limited -> unlimited: everyone waiting is released at once.
        l.set_download_bps(NonZeroU32::new(1_000));
        tokio::time::sleep(Duration::from_secs(2)).await;
        let before = counter.load(Ordering::Relaxed);
        l.set_download_bps(None);
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(counter.load(Ordering::Relaxed) - before >= 20 * 16384);
    }

    #[tokio::test(start_paused = true)]
    async fn limits_below_one_block_and_bursts() {
        // 10 KB/s with 16 KiB blocks: blocks are paced, never refused.
        let l = limits(10_000);
        let counter = consumers(&l, 3);
        let s = per_second(&counter, 20).await;
        let total: u64 = s.iter().sum();
        assert!((180_000..=220_000).contains(&total), "{total} {s:?}");

        // After an idle period only 200 ms worth of tokens is saved up.
        let l = limits(1_000_000);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let start = Instant::now();
        for _ in 0..(1_200_000 / 16384) {
            l.prepare_for_download(BLOCK).await.unwrap();
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(950), "{elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn abandoned_reservations_are_given_back() {
        let l = limits(16384);
        // A waiter that gives up after 100 ms...
        let w = tokio::time::timeout(Duration::from_millis(100), l.prepare_for_download(BLOCK));
        assert!(w.await.is_err());
        // ...does not delay the next one past one block time.
        let start = Instant::now();
        l.prepare_for_download(BLOCK).await.unwrap();
        assert!(
            start.elapsed() <= Duration::from_millis(1000),
            "{:?}",
            start.elapsed()
        );
        l.set_download_bps(None);
        assert_eq!(l.get_config(), LimitsConfig::default());
    }
}
