//! When a torrent's `get_peers` lookup runs again.
//!
//! The DHT crate does not know what a torrent needs: the caller supplies the
//! interval between lookup rounds as a function of its own state, and wakes
//! the pacer when that state changed in a way that may shorten the wait (for
//! example the torrent lost peers). A wake only re-evaluates the interval; the
//! wait still counts from the end of the last round, so repeated wakes never
//! cause more lookups than the interval allows.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

type IntervalFn = dyn Fn() -> Duration + Send + Sync;

/// Interval provider and early wake for the lookup loops of one torrent
/// (shared by its IPv4 and IPv6 loops and by both hashes of a hybrid).
pub struct LookupPacer {
    interval: Box<IntervalFn>,
    wake: watch::Sender<()>,
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
            wake: watch::Sender::new(()),
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
        self.wake.send_replace(());
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.wake.subscribe()
    }

    /// Wait until `round_end + interval` (capped by `cap`), re-evaluating the
    /// interval whenever the pacer is woken.
    pub(crate) async fn wait_next_round(
        &self,
        wake_rx: &mut watch::Receiver<()>,
        round_end: Instant,
        cap: Option<Duration>,
    ) {
        loop {
            wake_rx.borrow_and_update();
            let mut d = self.interval();
            if let Some(cap) = cap {
                d = d.min(cap);
            }
            let deadline = round_end + d;
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return,
                r = wake_rx.changed() => {
                    if r.is_err() {
                        // The pacer is gone (cannot happen while `self`
                        // lives); finish the current wait.
                        tokio::time::sleep_until(deadline).await;
                        return;
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

    fn spawn_wait(p: &Arc<LookupPacer>, cap: Option<Duration>) -> tokio::task::JoinHandle<Instant> {
        let p = p.clone();
        let mut rx = p.subscribe();
        let start = Instant::now();
        tokio::spawn(async move {
            p.wait_next_round(&mut rx, start, cap).await;
            Instant::now()
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
        let end = h.await.unwrap();
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
        let end = h.await.unwrap();
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
        let end = h.await.unwrap();
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
        let end = h.await.unwrap();
        assert!(end - start <= Duration::from_secs(31));
    }
}
