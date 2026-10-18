//! A session-wide pool of open file handles with LRU eviction.
//!
//! Instead of keeping every file of every torrent open (which runs out of file
//! descriptors and memory for torrents with thousands of files), the filesystem
//! storage opens files lazily and parks the handles here. When the pool is over
//! capacity the least recently used handles are dropped; a handle that is in use
//! at that moment stays open until its user is done with it (it's an `Arc`).

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    sync::Arc,
};

use parking_lot::Mutex;

/// (torrent storage id, file index)
pub(crate) type PoolKey = (u64, usize);

/// An open file handle.
#[derive(Debug)]
pub(crate) struct PooledFile {
    pub file: File,
    pub writable: bool,
}

struct Entry<T> {
    value: Arc<T>,
    tick: u64,
}

struct Inner<T> {
    capacity: usize,
    tick: u64,
    map: HashMap<PoolKey, Entry<T>>,
    // tick -> key, oldest first.
    order: BTreeMap<u64, PoolKey>,
}

impl<T> Inner<T> {
    fn touch(&mut self, key: PoolKey) -> Option<Arc<T>> {
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(&key)?;
        self.order.remove(&e.tick);
        e.tick = tick;
        self.order.insert(tick, key);
        Some(e.value.clone())
    }

    fn remove(&mut self, key: PoolKey) -> Option<Arc<T>> {
        let e = self.map.remove(&key)?;
        self.order.remove(&e.tick);
        Some(e.value)
    }

    fn evict_over_capacity(&mut self, evicted: &mut Vec<Arc<T>>) {
        while self.map.len() > self.capacity {
            let Some((_, key)) = self.order.pop_first() else {
                break;
            };
            if let Some(e) = self.map.remove(&key) {
                evicted.push(e.value);
            }
        }
    }
}

/// LRU cache of `Arc<T>` keyed by [`PoolKey`]. Generic so it can be unit-tested
/// without opening files.
pub(crate) struct LruPool<T> {
    inner: Mutex<Inner<T>>,
}

impl<T> LruPool<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                capacity: capacity.max(1),
                tick: 0,
                map: HashMap::new(),
                order: BTreeMap::new(),
            }),
        }
    }

    /// Get an entry and mark it as most recently used.
    pub fn get(&self, key: PoolKey) -> Option<Arc<T>> {
        self.inner.lock().touch(key)
    }

    /// Insert (or replace) an entry as the most recently used one, evicting the
    /// least recently used entries over capacity.
    pub fn insert(&self, key: PoolKey, value: Arc<T>) {
        let mut evicted = Vec::new();
        {
            let mut g = self.inner.lock();
            if let Some(old) = g.remove(key) {
                evicted.push(old);
            }
            g.tick += 1;
            let tick = g.tick;
            g.map.insert(key, Entry { value, tick });
            g.order.insert(tick, key);
            g.evict_over_capacity(&mut evicted);
        }
        // Closing files happens outside the lock.
        drop(evicted);
    }

    pub fn remove(&self, key: PoolKey) {
        let removed = self.inner.lock().remove(key);
        drop(removed);
    }

    /// Remove all entries of one owner (torrent storage).
    pub fn remove_owner(&self, owner: u64) {
        let mut removed = Vec::new();
        {
            let mut g = self.inner.lock();
            let keys: Vec<PoolKey> = g.map.keys().filter(|k| k.0 == owner).copied().collect();
            for k in keys {
                if let Some(v) = g.remove(k) {
                    removed.push(v);
                }
            }
        }
        drop(removed);
    }

    pub fn clear(&self) {
        let removed: Vec<_> = {
            let mut g = self.inner.lock();
            g.order.clear();
            g.map.drain().map(|(_, e)| e.value).collect()
        };
        drop(removed);
    }

    pub fn set_capacity(&self, capacity: usize) {
        let mut evicted = Vec::new();
        {
            let mut g = self.inner.lock();
            g.capacity = capacity.max(1);
            g.evict_over_capacity(&mut evicted);
        }
        drop(evicted);
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.inner.lock().capacity
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map.len()
    }

    #[cfg(test)]
    fn keys_lru_first(&self) -> Vec<PoolKey> {
        self.inner.lock().order.values().copied().collect()
    }
}

pub(crate) type FilePool = LruPool<PooledFile>;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::LruPool;

    #[test]
    fn test_lru_evicts_least_recently_used() {
        let pool = LruPool::<u32>::new(3);
        pool.insert((1, 0), Arc::new(0));
        pool.insert((1, 1), Arc::new(1));
        pool.insert((1, 2), Arc::new(2));
        assert_eq!(pool.len(), 3);

        // Touch (1,0) so that (1,1) becomes the oldest.
        assert_eq!(*pool.get((1, 0)).unwrap(), 0);
        pool.insert((1, 3), Arc::new(3));
        assert_eq!(pool.len(), 3);
        assert!(pool.get((1, 1)).is_none());
        assert!(pool.get((1, 0)).is_some());
        assert!(pool.get((1, 2)).is_some());
        assert!(pool.get((1, 3)).is_some());
        assert_eq!(pool.keys_lru_first(), vec![(1, 0), (1, 2), (1, 3)]);
    }

    #[test]
    fn test_lru_replace_keeps_single_entry() {
        let pool = LruPool::<u32>::new(2);
        pool.insert((1, 0), Arc::new(0));
        pool.insert((1, 0), Arc::new(10));
        assert_eq!(pool.len(), 1);
        assert_eq!(*pool.get((1, 0)).unwrap(), 10);
    }

    #[test]
    fn test_lru_in_use_handle_survives_eviction() {
        let pool = LruPool::<u32>::new(1);
        pool.insert((1, 0), Arc::new(0));
        let in_use = pool.get((1, 0)).unwrap();
        pool.insert((1, 1), Arc::new(1));
        assert!(pool.get((1, 0)).is_none());
        // The evicted value is still usable by whoever holds it.
        assert_eq!(*in_use, 0);
        assert_eq!(Arc::strong_count(&in_use), 1);
    }

    #[test]
    fn test_lru_remove_owner_and_capacity() {
        let pool = LruPool::<u32>::new(10);
        for i in 0..4 {
            pool.insert((1, i), Arc::new(0));
            pool.insert((2, i), Arc::new(0));
        }
        assert_eq!(pool.len(), 8);
        pool.remove_owner(1);
        assert_eq!(pool.len(), 4);
        assert!(pool.get((1, 0)).is_none());
        assert!(pool.get((2, 0)).is_some());

        // (2,0) was just touched, so shrinking keeps it and the newest entries.
        pool.set_capacity(2);
        assert_eq!(pool.capacity(), 2);
        assert_eq!(pool.len(), 2);
        assert!(pool.get((2, 0)).is_some());
        assert!(pool.get((2, 3)).is_some());

        pool.remove((2, 3));
        assert_eq!(pool.len(), 1);
        pool.clear();
        assert_eq!(pool.len(), 0);

        // Capacity is at least 1.
        pool.set_capacity(0);
        assert_eq!(pool.capacity(), 1);
    }
}
