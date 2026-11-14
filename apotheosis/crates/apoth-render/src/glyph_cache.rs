// SPDX-License-Identifier: AGPL-3.0-only
//! Process-wide CPU glyph cache (T-07 §4 Atlases): immutable `Arc` entries in a sharded map,
//! shared by every window so a glyph is rasterised once per process. Built-in glyphs (box
//! drawing, blocks, braille, powerline) are cached per (cell size, code point).

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::{Arc, Mutex};

const SHARDS: usize = 16;

/// Sharded map of immutable shared entries.
#[derive(Debug)]
pub struct SharedCache<K, V> {
    hasher: RandomState,
    shards: [Mutex<HashMap<K, Arc<V>>>; SHARDS],
}

impl<K: Hash + Eq, V> Default for SharedCache<K, V> {
    fn default() -> Self {
        Self {
            hasher: RandomState::new(),
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
        }
    }
}

impl<K: Hash + Eq, V> SharedCache<K, V> {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn shard(&self, k: &K) -> &Mutex<HashMap<K, Arc<V>>> {
        let h = self.hasher.hash_one(k);
        &self.shards[(h as usize) % SHARDS]
    }

    /// The entry for `key`, built by `make` on a miss. `make` runs outside the shard lock; if
    /// two threads race, the first insert wins and both get the same `Arc`.
    pub fn get_or_insert_with(&self, key: K, make: impl FnOnce() -> V) -> Arc<V>
    where
        K: Clone,
    {
        let shard = self.shard(&key);
        if let Some(v) = lock(shard).get(&key) {
            return Arc::clone(v);
        }
        let built = Arc::new(make());
        Arc::clone(lock(shard).entry(key).or_insert(built))
    }

    /// The entry for `key` if present. Takes the shard lock only for the lookup.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        lock(self.shard(key)).get(key).map(Arc::clone)
    }

    /// Entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| lock(s).len()).sum()
    }

    /// No entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Key of a built-in glyph: cell width, cell height (px), code point.
pub type BuiltinKey = (u16, u16, u32);

/// Cache of rasterised built-in glyph masks (R8, row-major, `w * h` bytes).
pub type BuiltinCache = SharedCache<BuiltinKey, Vec<u8>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn second_lookup_shares_the_first_arc() {
        let c: BuiltinCache = SharedCache::new();
        let n = AtomicUsize::new(0);
        let mk = || {
            n.fetch_add(1, Ordering::SeqCst);
            vec![0xff; 8 * 16]
        };
        let a = c.get_or_insert_with((8, 16, 0x2588), mk);
        let b = c.get_or_insert_with((8, 16, 0x2588), mk);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        let d = c.get_or_insert_with((9, 16, 0x2588), mk);
        assert!(!Arc::ptr_eq(&a, &d));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn shared_across_threads() {
        let c = Arc::new(SharedCache::<u32, u32>::new());
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let c = Arc::clone(&c);
                std::thread::spawn(move || {
                    (0..100)
                        .map(|k| *c.get_or_insert_with(k, || k * 2))
                        .sum::<u32>()
                })
            })
            .collect();
        for h in hs {
            assert_eq!(h.join().unwrap(), (0..100).map(|k| k * 2).sum::<u32>());
        }
        assert_eq!(c.len(), 100);
    }
}
