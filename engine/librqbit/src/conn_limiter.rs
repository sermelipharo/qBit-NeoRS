//! A connection limiter whose limit can change at runtime.
//!
//! It wraps a tokio semaphore. Raising the limit adds permits. Lowering it
//! removes available permits right away; whatever can't be removed yet
//! (because those connections are alive) is recorded as debt, and permits
//! returned by closing connections pay the debt instead of going back to the
//! semaphore. No background task is needed.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

/// The limit used for "unlimited".
pub(crate) const UNLIMITED: usize = Semaphore::MAX_PERMITS;

struct Inner {
    sem: Arc<Semaphore>,
    // The configured limit (UNLIMITED for none). Changes are serialized by this lock.
    limit: Mutex<usize>,
    debt: AtomicUsize,
}

impl Inner {
    // Returns true if the permit was consumed to pay debt.
    fn try_pay_debt(&self) -> bool {
        self.debt
            .try_update(Ordering::AcqRel, Ordering::Acquire, |d| d.checked_sub(1))
            .is_ok()
    }
}

#[derive(Clone)]
pub(crate) struct ConnLimiter {
    inner: Arc<Inner>,
}

/// A held connection slot. Dropping it frees the slot (or pays debt).
pub(crate) struct ConnPermit {
    permit: Option<OwnedSemaphorePermit>,
    inner: Arc<Inner>,
}

impl Drop for ConnPermit {
    fn drop(&mut self) {
        if let Some(p) = self.permit.take()
            && self.inner.try_pay_debt()
        {
            p.forget();
        }
    }
}

impl ConnLimiter {
    /// `limit`: `None` for unlimited.
    pub fn new(limit: Option<usize>) -> Self {
        let limit = limit.unwrap_or(UNLIMITED).min(UNLIMITED);
        Self {
            inner: Arc::new(Inner {
                sem: Arc::new(Semaphore::new(limit)),
                limit: Mutex::new(limit),
                debt: AtomicUsize::new(0),
            }),
        }
    }

    /// The configured limit, `None` if unlimited.
    pub fn limit(&self) -> Option<usize> {
        let l = *self.inner.limit.lock();
        (l != UNLIMITED).then_some(l)
    }

    /// Change the limit. Existing connections are kept; when the limit was
    /// lowered below the number of live connections, new ones are refused
    /// until enough of them close.
    pub fn set_limit(&self, limit: Option<usize>) {
        let new = limit.unwrap_or(UNLIMITED).min(UNLIMITED);
        let mut g = self.inner.limit.lock();
        let old = *g;
        if new > old {
            let mut add = new - old;
            // Cancel debt first (atomically, as dropped permits pay it concurrently).
            let prev_debt = self
                .inner
                .debt
                .try_update(Ordering::AcqRel, Ordering::Acquire, |d| {
                    Some(d - d.min(add))
                })
                .unwrap_or_default();
            add -= prev_debt.min(add);
            // Never exceed the semaphore maximum.
            let room = UNLIMITED.saturating_sub(self.inner.sem.available_permits());
            self.inner.sem.add_permits(add.min(room));
        } else if new < old {
            let reduce = old - new;
            let forgotten = self.inner.sem.forget_permits(reduce);
            self.inner
                .debt
                .fetch_add(reduce - forgotten, Ordering::AcqRel);
        }
        *g = new;
    }

    pub async fn acquire(&self) -> Result<ConnPermit, AcquireError> {
        let permit = self.inner.sem.clone().acquire_owned().await?;
        Ok(ConnPermit {
            permit: Some(permit),
            inner: self.inner.clone(),
        })
    }

    pub fn try_acquire(&self) -> Option<ConnPermit> {
        let permit = self.inner.sem.clone().try_acquire_owned().ok()?;
        Some(ConnPermit {
            permit: Some(permit),
            inner: self.inner.clone(),
        })
    }

    /// Number of connection slots in use (approximate while debt is pending).
    #[cfg(test)]
    fn in_use(&self) -> usize {
        let limit = *self.inner.limit.lock();
        limit + self.inner.debt.load(Ordering::Acquire) - self.inner.sem.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_up_and_down() {
        let l = ConnLimiter::new(Some(2));
        let a = l.try_acquire().unwrap();
        let b = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());
        assert_eq!(l.in_use(), 2);

        l.set_limit(Some(3));
        let c = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());

        // Lower to 1 while 3 are in use: 2 permits of debt.
        l.set_limit(Some(1));
        assert_eq!(l.limit(), Some(1));
        drop(a);
        assert!(l.try_acquire().is_none());
        drop(b);
        assert!(l.try_acquire().is_none());
        drop(c);
        // Now one slot is free again.
        let d = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());

        // Raising cancels debt before adding permits.
        l.set_limit(Some(0));
        l.set_limit(Some(2));
        let e = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());
        drop(d);
        drop(e);
        assert_eq!(l.in_use(), 0);
    }

    #[test]
    fn unlimited_round_trip() {
        let l = ConnLimiter::new(None);
        assert_eq!(l.limit(), None);
        let held: Vec<_> = (0..10).map(|_| l.try_acquire().unwrap()).collect();
        l.set_limit(Some(5));
        assert!(l.try_acquire().is_none());
        drop(held);
        let held: Vec<_> = (0..5).map(|_| l.try_acquire().unwrap()).collect();
        assert!(l.try_acquire().is_none());
        l.set_limit(None);
        assert!(l.try_acquire().is_some());
        drop(held);
    }
}
