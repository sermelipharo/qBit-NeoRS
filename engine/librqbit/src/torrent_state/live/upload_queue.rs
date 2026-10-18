// Per-peer bookkeeping of the chunk requests a peer has sent us (uploads).
//
// A peer may only have `MAX_UPLOAD_REQUESTS` requests outstanding (we
// advertise this as `reqq` in the extended handshake, like libtorrent's
// `max_allowed_in_request_queue`). Requests above the cap are dropped, and a
// peer that keeps sending them is disconnected. `Cancel` removes a request
// that has not been sent yet, and a request leaves the queue when its chunk is
// read for sending, so the queue (and the writer channel behind it) stays
// bounded even if the peer stops reading.

use std::collections::HashSet;

/// The most chunk requests a peer may have outstanding with us.
pub(crate) const MAX_UPLOAD_REQUESTS: u32 = 500;

/// Requests above the cap a peer may send over a connection before it is
/// disconnected (clients that ignore `reqq` now and then are tolerated).
pub(crate) const MAX_DROPPED_UPLOAD_REQUESTS: u32 = 2000;

/// The most chunk requests handed to the peer's writer and not taken back
/// yet. A cancelled request stays in the writer's queue until its turn comes,
/// so without this a peer could cancel and re-request the same chunk to grow
/// that queue without bound while it stops reading (the writer is then
/// blocked and nothing is taken).
pub(crate) const MAX_UPLOAD_REQUESTS_IN_WRITER: u32 = 2 * MAX_UPLOAD_REQUESTS;

/// The most `ut_metadata` pieces queued for sending to one peer.
pub(crate) const MAX_QUEUED_METADATA_PIECES: u32 = 32;
/// BEP 52 `hashes` (up to ~17 KiB each) or `hash reject` replies queued
/// for one peer.
pub(crate) const MAX_QUEUED_HASH_REPLIES: u32 = 8;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PushResult {
    Queued,
    /// The same chunk is already queued.
    Duplicate,
    /// The queue is full: the request is dropped.
    Dropped,
    /// The peer sent too many requests over the cap.
    Abusive,
}

#[derive(Default)]
pub(crate) struct UploadQueue {
    // Absolute chunk indices.
    pending: HashSet<u32>,
    dropped: u32,
    // Requests sent to the writer, cancelled or not, not taken yet.
    in_writer: u32,
}

impl UploadQueue {
    pub fn push(&mut self, absolute_chunk: u32) -> PushResult {
        if self.pending.contains(&absolute_chunk) {
            return PushResult::Duplicate;
        }
        if self.pending.len() >= MAX_UPLOAD_REQUESTS as usize
            || self.in_writer >= MAX_UPLOAD_REQUESTS_IN_WRITER
        {
            self.dropped = self.dropped.saturating_add(1);
            if self.dropped > MAX_DROPPED_UPLOAD_REQUESTS {
                return PushResult::Abusive;
            }
            return PushResult::Dropped;
        }
        self.pending.insert(absolute_chunk);
        self.in_writer += 1;
        PushResult::Queued
    }

    /// Whether the request is still wanted (not cancelled), without taking it.
    pub fn contains(&self, absolute_chunk: u32) -> bool {
        self.pending.contains(&absolute_chunk)
    }

    /// Removes a request (cancelled). Returns whether it was queued.
    pub fn remove(&mut self, absolute_chunk: u32) -> bool {
        self.pending.remove(&absolute_chunk)
    }

    /// The writer reached a request it was handed by `push` (Queued): it
    /// leaves the writer's queue, and is sent if still wanted (returns
    /// whether it was, i.e. not cancelled).
    pub fn take(&mut self, absolute_chunk: u32) -> bool {
        self.in_writer = self.in_writer.saturating_sub(1);
        self.pending.remove(&absolute_chunk)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_outstanding_requests() {
        let mut q = UploadQueue::default();
        for i in 0..MAX_UPLOAD_REQUESTS {
            assert_eq!(q.push(i), PushResult::Queued);
        }
        assert_eq!(q.len(), MAX_UPLOAD_REQUESTS as usize);
        assert_eq!(q.push(1_000_000), PushResult::Dropped);
        assert_eq!(q.len(), MAX_UPLOAD_REQUESTS as usize);
        // Sending one frees a slot.
        assert!(q.take(0));
        assert_eq!(q.push(1_000_000), PushResult::Queued);
    }

    #[test]
    fn cancel_and_request_again_cannot_grow_the_writer_queue() {
        let mut q = UploadQueue::default();
        // The writer is blocked (takes nothing): request + cancel churn on
        // one chunk keeps the set tiny but fills the writer's queue.
        let mut queued = 0;
        let mut abusive = false;
        for _ in 0..(MAX_UPLOAD_REQUESTS_IN_WRITER + MAX_DROPPED_UPLOAD_REQUESTS + 10) {
            match q.push(42) {
                PushResult::Queued => queued += 1,
                PushResult::Abusive => {
                    abusive = true;
                    break;
                }
                _ => {}
            }
            q.remove(42);
        }
        assert_eq!(queued, MAX_UPLOAD_REQUESTS_IN_WRITER);
        assert!(abusive);
        // The writer catching up frees room again.
        let mut q = UploadQueue::default();
        for _ in 0..MAX_UPLOAD_REQUESTS_IN_WRITER {
            assert_eq!(q.push(1), PushResult::Queued);
            q.remove(1);
        }
        assert_eq!(q.push(1), PushResult::Dropped);
        assert!(!q.take(1));
        assert_eq!(q.push(1), PushResult::Queued);
        assert!(q.take(1));
    }

    #[test]
    fn duplicates_are_not_queued_twice() {
        let mut q = UploadQueue::default();
        assert_eq!(q.push(7), PushResult::Queued);
        assert_eq!(q.push(7), PushResult::Duplicate);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn cancel_removes_pending_request() {
        let mut q = UploadQueue::default();
        q.push(3);
        assert!(q.remove(3));
        assert!(!q.contains(3));
        // A cancel for something never requested is harmless.
        assert!(!q.remove(4));
    }

    #[test]
    fn flooding_past_the_cap_is_abusive() {
        let mut q = UploadQueue::default();
        for i in 0..MAX_UPLOAD_REQUESTS {
            q.push(i);
        }
        let base = MAX_UPLOAD_REQUESTS;
        for i in 0..MAX_DROPPED_UPLOAD_REQUESTS {
            assert_eq!(q.push(base + i), PushResult::Dropped);
        }
        assert_eq!(q.push(u32::MAX), PushResult::Abusive);
    }
}
