// SPDX-License-Identifier: AGPL-3.0-only

//! LRU of directory listings (FOG §Performance model, technique 2).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use fog_proto::{Entry, Kind};

/// One cached listing. `order` indexes `entries`; `entries` is in the same
/// order every client holding this `generation` has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub dir: u64,
    pub generation: u64,
    pub entries: Vec<Entry>,
    pub order: Vec<u32>,
}

impl Listing {
    /// Approximate heap footprint, used for the byte budget.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .entries
                .iter()
                .map(|e| std::mem::size_of::<Entry>() + e.name.capacity())
                .sum::<usize>()
            + self.order.capacity() * std::mem::size_of::<u32>()
    }
}

struct Slot {
    listing: Arc<Listing>,
    tick: u64,
    bytes: usize,
}

/// Listings keyed by raw path bytes, bounded by directory count and bytes.
/// A single listing larger than the byte budget is not retained.
pub struct Cache {
    max_dirs: usize,
    max_bytes: usize,
    bytes: usize,
    tick: u64,
    slots: HashMap<Vec<u8>, Slot>,
    lru: BTreeMap<u64, Vec<u8>>,
}

impl Cache {
    pub const DEFAULT_DIRS: usize = 256;
    pub const DEFAULT_BYTES: usize = 64 << 20;

    pub fn new(max_dirs: usize, max_bytes: usize) -> Self {
        Self {
            max_dirs,
            max_bytes,
            bytes: 0,
            tick: 0,
            slots: HashMap::new(),
            lru: BTreeMap::new(),
        }
    }

    /// Look up and mark most recently used.
    pub fn get(&mut self, path: &[u8]) -> Option<Arc<Listing>> {
        self.tick += 1;
        let slot = self.slots.get_mut(path)?;
        let key = self.lru.remove(&slot.tick)?;
        slot.tick = self.tick;
        self.lru.insert(self.tick, key);
        Some(slot.listing.clone())
    }

    /// Look up without touching recency.
    pub fn peek(&self, path: &[u8]) -> Option<&Arc<Listing>> {
        self.slots.get(path).map(|s| &s.listing)
    }

    /// Insert or replace as most recently used, then evict to fit.
    pub fn insert(&mut self, path: Vec<u8>, listing: Arc<Listing>) {
        self.remove(&path);
        self.tick += 1;
        let bytes = listing.bytes() + path.len();
        self.bytes += bytes;
        self.lru.insert(self.tick, path.clone());
        self.slots.insert(
            path,
            Slot {
                listing,
                tick: self.tick,
                bytes,
            },
        );
        while self.slots.len() > self.max_dirs || self.bytes > self.max_bytes {
            let Some((_, oldest)) = self.lru.pop_first() else {
                break;
            };
            if let Some(s) = self.slots.remove(&oldest) {
                self.bytes -= s.bytes;
            }
        }
    }

    pub fn remove(&mut self, path: &[u8]) -> Option<Arc<Listing>> {
        let s = self.slots.remove(path)?;
        self.lru.remove(&s.tick);
        self.bytes -= s.bytes;
        Some(s.listing)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Current accounted size in bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DIRS, Self::DEFAULT_BYTES)
    }
}

/// What turns `old` into `new` under [`fog_proto::apply_diff`]: names that
/// left (or changed kind), and entries that arrived (or changed kind).
pub fn diff(old: &[Entry], new: &[Entry]) -> (Vec<Vec<u8>>, Vec<Entry>) {
    let o: HashMap<&[u8], Kind> = old.iter().map(|e| (e.name.as_slice(), e.kind)).collect();
    let n: HashMap<&[u8], Kind> = new.iter().map(|e| (e.name.as_slice(), e.kind)).collect();
    let removed = old
        .iter()
        .filter(|e| n.get(e.name.as_slice()) != Some(&e.kind))
        .map(|e| e.name.clone())
        .collect();
    let added = new
        .iter()
        .filter(|e| o.get(e.name.as_slice()) != Some(&e.kind))
        .cloned()
        .collect();
    (removed, added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn e(n: &str, k: Kind) -> Entry {
        Entry {
            name: n.as_bytes().to_vec(),
            kind: k,
        }
    }

    fn listing(dir: u64, names: usize, len: usize) -> Arc<Listing> {
        Arc::new(Listing {
            dir,
            generation: 0,
            entries: (0..names)
                .map(|i| Entry {
                    name: vec![b'a' + (i % 26) as u8; len],
                    kind: Kind::File,
                })
                .collect(),
            order: (0..names as u32).collect(),
        })
    }

    #[test]
    fn evicts_by_count_lru_first() {
        let mut c = Cache::new(2, usize::MAX);
        c.insert(b"/a".to_vec(), listing(1, 1, 1));
        c.insert(b"/b".to_vec(), listing(2, 1, 1));
        assert_eq!(c.get(b"/a").unwrap().dir, 1); // /a now MRU
        c.insert(b"/c".to_vec(), listing(3, 1, 1));
        assert_eq!(c.len(), 2);
        assert!(c.peek(b"/b").is_none());
        assert!(c.peek(b"/a").is_some() && c.peek(b"/c").is_some());
    }

    #[test]
    fn evicts_by_bytes() {
        let one = listing(0, 100, 100).bytes() + 2;
        let mut c = Cache::new(usize::MAX, one * 2 + one / 2);
        c.insert(b"/a".to_vec(), listing(1, 100, 100));
        c.insert(b"/b".to_vec(), listing(2, 100, 100));
        assert_eq!(c.len(), 2);
        assert_eq!(c.bytes(), 2 * one);
        c.insert(b"/c".to_vec(), listing(3, 100, 100));
        assert_eq!(c.len(), 2);
        assert!(c.peek(b"/a").is_none());
        assert_eq!(c.bytes(), 2 * one);

        // Larger than the whole budget: not retained, and nothing leaks.
        let mut tiny = Cache::new(8, 16);
        tiny.insert(b"/x".to_vec(), listing(1, 10, 10));
        assert!(tiny.is_empty());
        assert_eq!(tiny.bytes(), 0);
    }

    #[test]
    fn replace_and_remove_keep_accounting() {
        let mut c = Cache::new(4, usize::MAX);
        c.insert(b"/a".to_vec(), listing(1, 10, 10));
        c.insert(b"/a".to_vec(), listing(2, 1, 1));
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), listing(0, 1, 1).bytes() + 2);
        assert_eq!(c.remove(b"/a").unwrap().dir, 2);
        assert_eq!(c.bytes(), 0);
        assert!(c.get(b"/a").is_none());
    }

    #[test]
    fn diff_round_trips_with_kind_change() {
        let old = vec![
            e("keep", Kind::File),
            e("gone", Kind::File),
            e("morph", Kind::File),
        ];
        let new = vec![
            e("morph", Kind::Dir),
            e("fresh", Kind::Symlink),
            e("keep", Kind::File),
        ];
        let (removed, added) = diff(&old, &new);
        let rs: HashSet<&[u8]> = removed.iter().map(Vec::as_slice).collect();
        assert_eq!(rs, HashSet::from([&b"gone"[..], b"morph"]));
        assert_eq!(added.len(), 2);
        assert!(added.contains(&e("morph", Kind::Dir)));

        let mut applied = old.clone();
        fog_proto::apply_diff(&mut applied, &removed, &added);
        let a: HashSet<Entry> = applied.into_iter().collect();
        let n: HashSet<Entry> = new.into_iter().collect();
        assert_eq!(a, n);

        assert_eq!(diff(&old, &old), (vec![], vec![]));
    }
}
