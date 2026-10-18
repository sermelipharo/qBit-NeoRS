//! Limit on how many torrents check (hash) their data at the same time, adjustable
//! at runtime ("maximum active checking torrents").

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

struct State {
    limit: Option<usize>,
    active: usize,
}

pub(crate) struct CheckingLimiter {
    state: Mutex<State>,
    notify: Notify,
}

/// Held while a check runs; frees the slot on drop.
pub(crate) struct CheckingPermit {
    limiter: Arc<CheckingLimiter>,
}

impl Drop for CheckingPermit {
    fn drop(&mut self) {
        self.limiter.state.lock().active -= 1;
        self.limiter.notify.notify_waiters();
    }
}

impl CheckingLimiter {
    /// `None` = unlimited.
    pub fn new(limit: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State { limit, active: 0 }),
            notify: Notify::new(),
        })
    }

    pub async fn acquire(self: &Arc<Self>) -> CheckingPermit {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Register before looking, so that a release in between isn't missed.
            notified.as_mut().enable();
            {
                let mut s = self.state.lock();
                if s.limit.is_none_or(|l| s.active < l) {
                    s.active += 1;
                    return CheckingPermit {
                        limiter: self.clone(),
                    };
                }
            }
            notified.await;
        }
    }

    pub fn set_limit(&self, limit: Option<usize>) {
        self.state.lock().limit = limit;
        self.notify.notify_waiters();
    }

    pub fn limit(&self) -> Option<usize> {
        self.state.lock().limit
    }

    #[cfg(test)]
    pub fn active(&self) -> usize {
        self.state.lock().active
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::CheckingLimiter;

    #[tokio::test]
    async fn test_limit_changes_at_runtime() {
        let l = CheckingLimiter::new(Some(1));
        let p1 = l.acquire().await;
        let waiting = tokio::spawn({
            let l = l.clone();
            async move {
                let _p = l.acquire().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        assert_eq!(l.active(), 1);

        // Raising the limit lets the waiter in while p1 is still held.
        l.set_limit(Some(2));
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();

        // Zero blocks everyone; unlimited lets everyone in.
        l.set_limit(Some(0));
        let blocked = tokio::spawn({
            let l = l.clone();
            async move {
                let _p = l.acquire().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!blocked.is_finished());
        l.set_limit(None);
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        let _many: Vec<_> = futures::future::join_all((0..10).map(|_| l.acquire())).await;
        assert_eq!(l.active(), 11);
        drop(p1);
        assert_eq!(l.limit(), None);
    }
}
