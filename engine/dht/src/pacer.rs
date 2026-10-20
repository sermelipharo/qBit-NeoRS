//! When a torrent's `get_peers` lookup runs again.
//!
//! The DHT crate does not know what a torrent needs: the caller supplies the
//! interval between lookup rounds as a function of its own state, and wakes
//! the pacer when that state changed in a way that may shorten the wait (for
//! example the torrent lost peers). A wake only re-evaluates the interval; the
//! wait still counts from the end of the last round, so repeated wakes never
//! cause more lookups than the interval allows. A forced round (a manual
//! reannounce) starts at once and announces regardless of the last one.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

type IntervalFn = dyn Fn() -> Duration + Send + Sync;

/// Interval provider and early wake for the lookup loops of one torrent
/// (shared by its IPv4 and IPv6 loops and by both hashes of a hybrid).
pub struct LookupPacer {
    interval: Box<IntervalFn>,
    /// Count of forced rounds; every change wakes the loops.
    wake: watch::Sender<u64>,
}

impl std::fmt::Debug for LookupPacer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LookupPacer").finish_non_exhaustive()
    }
}

impl LookupPacer {
    /// `interval` is called after every round and on every wake; keep it
    /// cheap (no locks held for long, no I/O).
    pub fn new(interval: impl Fn() -> Duration + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            interval: Box::new(interval),
            wake: watch::Sender::new(0),
        })
    }

    /// A constant interval.
    pub fn fixed(interval: Duration) -> Arc<Self> {
        Self::new(move || interval)
    }

    /// The current interval between rounds.
    pub fn interval(&self) -> Duration {
        (self.interval)()
    }

    /// Re-evaluate the interval in every waiting loop now. A loop whose
    /// new deadline (end of its last round + interval) has passed starts
    /// its next round at once.
    pub fn wake(&self) {
        self.wake.send_modify(|_| {});
    }

    /// Start a round in every loop now, announcing in it.
    pub fn force(&self) {
        self.wake.send_modify(|g| *g = g.wrapping_add(1));
    }

    /// Observe wakes (each [`Self::wake`] or [`Self::force`] marks the
    /// receiver changed; the value counts forced rounds).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.wake.subscribe()
    }

    /// Wait until `round_end + interval` (capped by `cap`), re-evaluating the
    /// interval whenever the pacer is woken. `forced_seen`: the force count
    /// this loop has handled. Returns true if the round was forced.
    pub(crate) async fn wait_next_round(
        &self,
        wake_rx: &mut watch::Receiver<u64>,
        forced_seen: &mut u64,
        round_end: Instant,
        cap: Option<Duration>,
    ) -> bool {
        loop {
            let forced = *wake_rx.borrow_and_update();
            if forced != *forced_seen {
                *forced_seen = forced;
                return true;
            }
            let mut d = self.interval();
            if let Some(cap) = cap {
                d = d.min(cap);
            }
            let deadline = round_end + d;
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return false,
                r = wake_rx.changed() => {
                    if r.is_err() {
                        // The pacer is gone (cannot happen while `self`
                        // lives); finish the current wait.
                        tokio::time::sleep_until(deadline).await;
                        return false;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn pacer(secs: &Arc<AtomicU64>) -> Arc<LookupPacer> {
        let s = secs.clone();
        LookupPacer::new(move || Duration::from_secs(s.load(Ordering::Relaxed)))
    }

    fn spawn_wait(
        p: &Arc<LookupPacer>,
        cap: Option<Duration>,
    ) -> tokio::task::JoinHandle<(Instant, bool)> {
        let p = p.clone();
        let mut rx = p.subscribe();
        let mut seen = *rx.borrow();
        let start = Instant::now();
        tokio::spawn(async move {
            let forced = p.wait_next_round(&mut rx, &mut seen, start, cap).await;
            (Instant::now(), forced)
        })
    }

    async fn advance(d: Duration) {
        tokio::time::advance(d).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn sleeps_for_the_interval() {
        let secs = Arc::new(AtomicU64::new(900));
        let p = pacer(&secs);
        let start = Instant::now();
        let h = spawn_wait(&p, None);
        advance(Duration::from_secs(899)).await;
        assert!(!h.is_finished());
        advance(Duration::from_secs(2)).await;
        let (end, forced) = h.await.unwrap();
        assert!(!forced);
        assert!(end - start >= Duration::from_secs(900));
    }

    #[tokio::test(start_paused = true)]
    async fn wake_shortens_the_wait_from_the_round_end() {
        let secs = Arc::new(AtomicU64::new(900));
        let p = pacer(&secs);
        let start = Instant::now();
        let h = spawn_wait(&p, None);
        advance(Duration::from_secs(10)).await;
        // Woken without a shorter interval: keeps sleeping.
        p.wake();
        advance(Duration::from_secs(60)).await;
        assert!(!h.is_finished());
        // Now 70 s after the round: a 60 s interval is due at once.
        secs.store(60, Ordering::Relaxed);
        p.wake();
        advance(Duration::from_millis(1)).await;
        let (end, forced) = h.await.unwrap();
        assert!(!forced);
        assert!(end - start < Duration::from_secs(71), "{:?}", end - start);
    }

    #[tokio::test(start_paused = true)]
    async fn wake_before_the_short_deadline_waits_for_it() {
        let secs = Arc::new(AtomicU64::new(900));
        let p = pacer(&secs);
        let start = Instant::now();
        let h = spawn_wait(&p, None);
        advance(Duration::from_secs(5)).await;
        secs.store(60, Ordering::Relaxed);
        p.wake();
        advance(Duration::from_secs(50)).await;
        assert!(!h.is_finished());
        advance(Duration::from_secs(6)).await;
        let (end, forced) = h.await.unwrap();
        assert!(!forced);
        let took = end - start;
        assert!(took >= Duration::from_secs(60) && took < Duration::from_secs(62));
    }

    #[tokio::test(start_paused = true)]
    async fn cap_limits_the_interval() {
        let secs = Arc::new(AtomicU64::new(900));
        let p = pacer(&secs);
        let start = Instant::now();
        let h = spawn_wait(&p, Some(Duration::from_secs(30)));
        advance(Duration::from_secs(31)).await;
        let (end, forced) = h.await.unwrap();
        assert!(!forced);
        assert!(end - start <= Duration::from_secs(31));
    }

    #[tokio::test(start_paused = true)]
    async fn force_starts_a_round_at_once() {
        let secs = Arc::new(AtomicU64::new(900));
        let p = pacer(&secs);
        let start = Instant::now();
        let a = spawn_wait(&p, None);
        let b = spawn_wait(&p, None);
        advance(Duration::from_secs(10)).await;
        p.force();
        advance(Duration::from_millis(1)).await;
        // Both loops (IPv4 and IPv6) see it.
        for h in [a, b] {
            let (end, forced) = h.await.unwrap();
            assert!(forced);
            assert!(end - start < Duration::from_secs(11));
        }
        // Handled once: the next wait sleeps again.
        let c = spawn_wait(&p, None);
        advance(Duration::from_secs(60)).await;
        assert!(!c.is_finished());
    }
}
