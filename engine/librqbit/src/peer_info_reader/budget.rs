//! The session-wide memory budget of data fetched from untrusted peers
//! before it can be verified as a whole: BEP 9 metadata pieces (checked
//! against the info hash only once complete) and BEP 52 piece layers.
//! Every byte kept is reserved first; a fetch that would go over the budget
//! fails instead of allocating, and reservations are returned when dropped.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Default budget, all torrents and peers together. The largest piece
/// layer (2^21 pieces) takes 64 MiB and the largest metadata we accept
/// 32 MiB: with two fetches of each per torrent, one torrent at the worst
/// fits.
pub(crate) const DEFAULT_FETCH_BUDGET: u64 = 192 * 1024 * 1024;

/// A byte budget shared by every metadata and layer fetch of a session.
#[derive(Debug)]
pub struct FetchBudget {
    limit: u64,
    used: AtomicU64,
}

impl FetchBudget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicU64::new(0),
        })
    }

    /// Bytes currently reserved.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    fn try_take(&self, n: u64) -> bool {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |u| {
                u.checked_add(n).filter(|t| *t <= self.limit)
            })
            .is_ok()
    }

    fn give(&self, n: u64) {
        let _ = self
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |u| {
                Some(u.saturating_sub(n))
            });
    }
}

/// The [`FetchBudget`] can't take more bytes.
#[derive(Debug, thiserror::Error)]
#[error("fetch memory budget exhausted")]
pub(crate) struct BudgetExhausted;

/// Bytes reserved from a [`FetchBudget`], returned when dropped.
#[derive(Debug)]
pub(crate) struct FetchReservation {
    budget: Arc<FetchBudget>,
    bytes: u64,
}

impl FetchReservation {
    pub fn new(budget: Arc<FetchBudget>) -> Self {
        Self { budget, bytes: 0 }
    }

    /// Reserves `n` more bytes, or fails ([`BudgetExhausted`]) without
    /// reserving anything.
    pub fn grow(&mut self, n: u64) -> anyhow::Result<()> {
        if !self.budget.try_take(n) {
            return Err(BudgetExhausted.into());
        }
        self.bytes += n;
        Ok(())
    }

    /// Returns up to `n` reserved bytes (memory handed on or freed).
    pub fn release(&mut self, n: u64) {
        let n = n.min(self.bytes);
        self.bytes -= n;
        self.budget.give(n);
    }
}

impl Drop for FetchReservation {
    fn drop(&mut self) {
        self.budget.give(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_reservations_are_returned() {
        let b = FetchBudget::new(100);
        let mut r = FetchReservation::new(b.clone());
        r.grow(60).unwrap();
        assert!(r.grow(41).is_err(), "over the limit");
        assert_eq!(b.used(), 60);
        let mut r2 = FetchReservation::new(b.clone());
        r2.grow(40).unwrap();
        assert!(r2.grow(1).is_err());
        drop(r);
        assert_eq!(b.used(), 40);
        drop(r2);
        assert_eq!(b.used(), 0);
        // Concurrent reservations never exceed the limit.
        let b = FetchBudget::new(1000);
        std::thread::scope(|s| {
            for _ in 0..8 {
                let b = b.clone();
                s.spawn(move || {
                    for _ in 0..1000 {
                        let mut r = FetchReservation::new(b.clone());
                        let _ = r.grow(300);
                        assert!(b.used() <= 1000);
                    }
                });
            }
        });
        assert_eq!(b.used(), 0);
    }
}
