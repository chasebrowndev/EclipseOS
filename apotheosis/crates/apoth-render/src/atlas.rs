// SPDX-License-Identifier: AGPL-3.0-only
//! Glyph atlas bookkeeping (T-07 §4; RENDERING-02, PERFORMANCE-17): skyline-packed pages,
//! LRU by the window's frame counter, eviction gated on frames in flight, the inline raster
//! budget and the deferred-miss tracker.
//!
//! This module owns placement and lifetime only; pixel upload belongs to the GPU executor,
//! which makes one [`Atlas`] for the R8 mask atlas and one for the RGBA colour atlas.
//! Eviction granularity is the page (skyline packing cannot free single rects): a page may be
//! reset only when every entry on it was last used before the oldest frame still in flight.
//! DELIBERATE DEVIATION from T-07 §4 (per-entry LRU): eviction is per page, so the atlas can
//! return `Full` where per-entry eviction would have freed a slot; the owner must either accept
//! this in the spec/an ADR or require a repacking atlas. The shared CPU glyph cache is
//! `glyph_cache`.
//! The page size and cap are not in T-07; defaults are 1024 and 8 pages.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::time::Duration;

/// Gutter between packed rects.
const PAD: u32 = 1;

/// A placed rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Page index.
    pub page: u16,
    /// Origin x.
    pub x: u16,
    /// Origin y.
    pub y: u16,
    /// Width.
    pub w: u16,
    /// Height.
    pub h: u16,
}

/// Why an insert failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtlasError {
    /// The rect cannot fit an empty page.
    TooLarge,
    /// Every page is full and none is evictable yet: draw without the glyph this frame.
    Full,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    x: u32,
    y: u32,
    w: u32,
}

#[derive(Debug)]
struct Skyline {
    w: u32,
    h: u32,
    nodes: Vec<Node>,
}

impl Skyline {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            nodes: vec![Node { x: 0, y: 0, w }],
        }
    }

    fn reset(&mut self) {
        self.nodes.clear();
        self.nodes.push(Node {
            x: 0,
            y: 0,
            w: self.w,
        });
    }

    #[allow(clippy::needless_range_loop)]
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let mut best: Option<(u32, u32, usize)> = None; // (y, bottom, node)
        for i in 0..self.nodes.len() {
            let x = self.nodes[i].x;
            if x + w > self.w {
                break;
            }
            let mut y = 0;
            let mut j = i;
            let mut covered;
            loop {
                let n = self.nodes[j];
                y = y.max(n.y);
                covered = n.x + n.w;
                j += 1;
                if covered >= x + w || j == self.nodes.len() {
                    break;
                }
            }
            if covered < x + w || y + h > self.h {
                continue;
            }
            let bottom = y + h;
            if best.is_none_or(|(_, bb, _)| bottom < bb) {
                best = Some((y, bottom, i));
            }
        }
        let (y, bottom, i) = best?;
        let x = self.nodes[i].x;
        self.nodes.insert(i, Node { x, y: bottom, w });
        let end = x + w;
        let k = i + 1;
        while k < self.nodes.len() {
            let n = self.nodes[k];
            if n.x >= end {
                break;
            }
            let shrink = end - n.x;
            if shrink >= n.w {
                self.nodes.remove(k);
            } else {
                self.nodes[k].x += shrink;
                self.nodes[k].w -= shrink;
                break;
            }
        }
        let mut m = 0;
        while m + 1 < self.nodes.len() {
            if self.nodes[m].y == self.nodes[m + 1].y {
                self.nodes[m].w += self.nodes[m + 1].w;
                self.nodes.remove(m + 1);
            } else {
                m += 1;
            }
        }
        Some((x, y))
    }
}

#[derive(Debug)]
struct PageState {
    sky: Skyline,
    /// Most recent last-use over the page's entries.
    last_use: u64,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    place: Placement,
    last_use: u64,
}

/// One atlas (mask or colour) of skyline pages with an LRU map.
#[derive(Debug)]
pub struct Atlas<K> {
    page_size: u32,
    max_pages: usize,
    pages: Vec<PageState>,
    map: HashMap<K, Entry>,
    frame: u64,
}

impl<K: Hash + Eq + Copy> Atlas<K> {
    /// A new atlas of up to `max_pages` square pages of `page_size` pixels.
    #[must_use]
    pub fn new(page_size: u32, max_pages: usize) -> Self {
        Self {
            page_size,
            max_pages: max_pages.max(1),
            pages: Vec::new(),
            map: HashMap::new(),
            frame: 0,
        }
    }

    /// Default configuration (1024 px pages, at most 8).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(1024, 8)
    }

    /// Starts a new frame and returns its number.
    pub fn begin_frame(&mut self) -> u64 {
        self.frame += 1;
        self.frame
    }

    /// The current frame number.
    #[must_use]
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Pages allocated so far (the executor creates a texture per page).
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// No entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Looks `key` up and marks it used this frame. No allocation, no lock.
    pub fn get(&mut self, key: &K) -> Option<Placement> {
        let frame = self.frame;
        let e = self.map.get_mut(key)?;
        e.last_use = frame;
        let page = &mut self.pages[usize::from(e.place.page)];
        page.last_use = page.last_use.max(frame);
        Some(e.place)
    }

    /// Places a `w` x `h` rect for `key`. `oldest_in_flight` is the number of the oldest frame
    /// still in flight (the current frame when none is); a page is reset only if everything
    /// on it was last used before that frame.
    pub fn insert(
        &mut self,
        key: K,
        w: u32,
        h: u32,
        oldest_in_flight: u64,
    ) -> Result<Placement, AtlasError> {
        if let Some(p) = self.get(&key) {
            return Ok(p);
        }
        let (aw, ah) = (w + PAD, h + PAD);
        if aw > self.page_size || ah > self.page_size {
            return Err(AtlasError::TooLarge);
        }
        let mut found = None;
        for (i, p) in self.pages.iter_mut().enumerate() {
            if let Some(xy) = p.sky.alloc(aw, ah) {
                found = Some((i, xy));
                break;
            }
        }
        if found.is_none() && self.pages.len() < self.max_pages {
            let mut sky = Skyline::new(self.page_size, self.page_size);
            let xy = sky.alloc(aw, ah).ok_or(AtlasError::TooLarge)?;
            self.pages.push(PageState {
                sky,
                last_use: self.frame,
            });
            found = Some((self.pages.len() - 1, xy));
        }
        if found.is_none() {
            let victim = self
                .pages
                .iter()
                .enumerate()
                .filter(|(_, p)| p.last_use < oldest_in_flight)
                .min_by_key(|(_, p)| p.last_use)
                .map(|(i, _)| i);
            let i = victim.ok_or(AtlasError::Full)?;
            self.pages[i].sky.reset();
            self.map.retain(|_, e| usize::from(e.place.page) != i);
            let xy = self.pages[i]
                .sky
                .alloc(aw, ah)
                .ok_or(AtlasError::TooLarge)?;
            found = Some((i, xy));
        }
        let (i, (x, y)) = found.ok_or(AtlasError::Full)?;
        let place = Placement {
            page: i as u16,
            x: x as u16,
            y: y as u16,
            w: w as u16,
            h: h as u16,
        };
        self.map.insert(
            key,
            Entry {
                place,
                last_use: self.frame,
            },
        );
        self.pages[i].last_use = self.pages[i].last_use.max(self.frame);
        Ok(place)
    }

    /// Drops everything (font generation change, device rebuild).
    pub fn clear(&mut self) {
        self.map.clear();
        for p in &mut self.pages {
            p.sky.reset();
        }
    }
}

/// The per-frame inline raster budget: up to `max_glyphs` glyphs or `max_time` of raster
/// time, whichever is hit first; echo rows are exempt (T-07 §4).
#[derive(Clone, Copy, Debug)]
pub struct MissBudget {
    max_glyphs: u32,
    max_time: Duration,
    used: u32,
    spent: Duration,
}

impl Default for MissBudget {
    fn default() -> Self {
        Self::new(16, Duration::from_millis(1))
    }
}

impl MissBudget {
    /// A budget.
    #[must_use]
    pub fn new(max_glyphs: u32, max_time: Duration) -> Self {
        Self {
            max_glyphs,
            max_time,
            used: 0,
            spent: Duration::ZERO,
        }
    }

    /// Starts a frame.
    pub fn reset(&mut self) {
        self.used = 0;
        self.spent = Duration::ZERO;
    }

    /// May a glyph be rasterised inline now?
    #[must_use]
    pub fn allow(&self, echo: bool) -> bool {
        echo || (self.used < self.max_glyphs && self.spent < self.max_time)
    }

    /// Records one inline raster that took `elapsed`.
    pub fn record(&mut self, elapsed: Duration) {
        self.used += 1;
        self.spent += elapsed;
    }

    /// Glyphs rasterised this frame.
    #[must_use]
    pub fn used(&self) -> u32 {
        self.used
    }
}

/// Keys handed to workers and not yet resolved (the renderer's own pending set).
#[derive(Debug)]
pub struct MissTracker<K> {
    requested: HashSet<K>,
}

impl<K: Hash + Eq + Copy> Default for MissTracker<K> {
    fn default() -> Self {
        Self {
            requested: HashSet::new(),
        }
    }
}

impl<K: Hash + Eq + Copy> MissTracker<K> {
    /// Marks `key` requested; true when the caller must enqueue it (first request).
    pub fn request(&mut self, key: K) -> bool {
        self.requested.insert(key)
    }

    /// A worker result landed for `key`; true when it was awaited.
    pub fn complete(&mut self, key: &K) -> bool {
        self.requested.remove(key)
    }

    /// Outstanding requests.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.requested.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_without_overlap() {
        let mut a: Atlas<u32> = Atlas::new(64, 1);
        a.begin_frame();
        let mut placed = Vec::new();
        let mut k = 0;
        loop {
            let (w, h) = (5 + k % 7, 6 + k % 5);
            match a.insert(k, w, h, 0) {
                Ok(p) => placed.push(p),
                Err(_) => break,
            }
            k += 1;
        }
        assert!(placed.len() > 20);
        for (i, p) in placed.iter().enumerate() {
            assert!(u32::from(p.x) + u32::from(p.w) <= 64 && u32::from(p.y) + u32::from(p.h) <= 64);
            for q in &placed[i + 1..] {
                let sep =
                    p.x + p.w <= q.x || q.x + q.w <= p.x || p.y + p.h <= q.y || q.y + q.h <= p.y;
                assert!(sep, "{p:?} overlaps {q:?}");
            }
        }
    }

    #[test]
    fn hit_returns_same_placement() {
        let mut a: Atlas<u32> = Atlas::new(64, 1);
        a.begin_frame();
        let p = a.insert(1, 8, 8, 0).unwrap();
        assert_eq!(a.get(&1), Some(p));
        assert_eq!(a.insert(1, 8, 8, 0), Ok(p));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn too_large_is_reported() {
        let mut a: Atlas<u32> = Atlas::new(16, 1);
        assert_eq!(a.insert(1, 16, 4, 0), Err(AtlasError::TooLarge));
    }

    #[test]
    fn eviction_never_frees_an_in_flight_entry() {
        // One 16x16 page holds exactly one 14x14 glyph.
        let mut a: Atlas<u32> = Atlas::new(16, 1);
        let f1 = a.begin_frame();
        let p1 = a.insert(1, 14, 14, f1).unwrap();
        // Frame 2 starts while frame 1 is still in flight: nothing evictable.
        let f2 = a.begin_frame();
        assert_eq!(a.insert(2, 14, 14, f1), Err(AtlasError::Full));
        assert_eq!(a.get(&1), Some(p1), "entry still present");
        // Frame 1 completes: oldest in flight is now frame 2; entry 1 was last used in
        // frame 2 by the get above, so it is still protected.
        assert_eq!(a.insert(2, 14, 14, f2), Err(AtlasError::Full));
        // Frame 3 with only frame 3 in flight: frame-2 use is older, page may be reset.
        let f3 = a.begin_frame();
        let p2 = a.insert(2, 14, 14, f3).unwrap();
        assert_eq!(p2.page, 0);
        assert_eq!(a.get(&1), None, "evicted");
        assert!(a.get(&2).is_some());
    }

    #[test]
    fn lru_evicts_the_coldest_page() {
        let mut a: Atlas<u32> = Atlas::new(16, 2);
        let f = a.begin_frame();
        a.insert(1, 14, 14, f).unwrap(); // page 0, frame 1
        let f = a.begin_frame();
        a.insert(2, 14, 14, f).unwrap(); // page 1, frame 2
        let f = a.begin_frame();
        let p = a.insert(3, 14, 14, f).unwrap(); // both older than frame 3: coldest = page 0
        assert_eq!(p.page, 0);
        assert!(a.get(&1).is_none() && a.get(&2).is_some());
    }

    #[test]
    fn clear_forgets_everything() {
        let mut a: Atlas<u32> = Atlas::new(32, 1);
        a.insert(1, 8, 8, 0).unwrap();
        a.clear();
        assert!(a.is_empty() && a.get(&1).is_none());
        assert!(a.insert(1, 8, 8, 0).is_ok());
    }

    #[test]
    fn inline_budget_by_count_and_time_with_echo_exempt() {
        let mut b = MissBudget::default();
        for _ in 0..16 {
            assert!(b.allow(false));
            b.record(Duration::from_micros(10));
        }
        assert!(!b.allow(false), "17th glyph is deferred");
        assert!(b.allow(true), "echo rows always rasterise inline");
        b.reset();
        assert!(b.allow(false));
        b.record(Duration::from_millis(2));
        assert!(!b.allow(false), "time budget exhausted");
        assert_eq!(b.used(), 1);
    }

    #[test]
    fn deferred_result_is_requested_once_and_completes() {
        let mut t: MissTracker<u32> = MissTracker::default();
        assert!(t.request(7));
        assert!(!t.request(7), "no duplicate worker job");
        assert_eq!(t.outstanding(), 1);
        assert!(t.complete(&7));
        assert!(!t.complete(&7));
        assert_eq!(t.outstanding(), 0);
    }
}
