// SPDX-License-Identifier: AGPL-3.0-only
//! Per-pane image store: one byte budget across kitty, sixel and iTerm2 images, tiered
//! eviction, content-hash sharing, main/alternate partitions (T-15 §4, GRAPHICS-13..15,
//! PERFORMANCE-51/52).
//!
//! Pins, tracked-pin anchoring and reflow are grid-side (M25 proper): the grid tells the
//! store which placements are on screen ([`ImageStore::set_visible`]) and answers the
//! positional delete selectors through a predicate.

use crate::kitty::{DeleteSpec, DeleteTarget, PlaceSpec};
use crate::{ErrCode, Image, Limits};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub type ImageId = u32;

/// Placements kept per pane; the oldest is dropped past this (T-15 §4).
pub const MAX_PLACEMENTS: usize = 4096;

/// Auto-assigned ids live in the upper half of the u32 space (T-15 §1).
pub const AUTO_ID_BASE: u32 = 0x8000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Screen {
    Main,
    Alt,
}

/// The per-process byte budget (`graphics.quota-total`), shared by every pane's store.
#[derive(Debug, Clone, Default)]
pub struct ProcessBudget(Arc<AtomicU64>);

impl ProcessBudget {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn used(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    pub(crate) fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    pub(crate) fn sub(&self, n: u64) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(n))
            });
    }
}

/// Process-wide blob serials: a renderer keys its texture cache by them, so a serial is never
/// reused for different pixels.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct Blob {
    image: Arc<Image>,
    hash: u64,
    refs: usize,
    serial: u64,
}

#[derive(Debug)]
struct Entry {
    blob: usize,
    last_use: u64,
}

/// One placement of an image. The grid owns geometry; the store owns lifetime.
#[derive(Debug, Clone)]
pub struct Placement {
    pub screen: Screen,
    pub image: ImageId,
    pub id: u32,
    pub spec: PlaceSpec,
    /// On screen right now (set by the grid). Referenced and visible images are never evicted.
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// Larger than the per-image cap.
    TooLarge,
    /// Over quota with nothing evictable: reply `ENOSPC`.
    NoSpace,
}

impl StoreError {
    pub fn code(self) -> ErrCode {
        match self {
            Self::TooLarge => ErrCode::Efbig,
            Self::NoSpace => ErrCode::Enospc,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Inserted {
    pub id: ImageId,
    /// Images evicted to make room.
    pub evicted: Vec<(Screen, ImageId)>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeleteReport {
    pub placements_removed: usize,
    /// Images whose data was freed (uppercase selectors).
    pub images_freed: Vec<ImageId>,
}

fn hash_image(img: &Image) -> u64 {
    // FNV-1a over dims and pixels; only a bucket key -- equality is verified bytewise.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in img
        .width
        .to_le_bytes()
        .iter()
        .chain(img.height.to_le_bytes().iter())
        .chain(&img.rgba)
    {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[derive(Debug)]
pub struct ImageStore {
    limits: Limits,
    budget: ProcessBudget,
    used: u64,
    blobs: Vec<Option<Blob>>,
    by_hash: HashMap<u64, Vec<usize>>,
    images: HashMap<(Screen, ImageId), Entry>,
    numbers: HashMap<(Screen, u32), ImageId>,
    placements: Vec<Placement>,
    seq: u64,
    next_auto: u32,
    next_auto_placement: u32,
}

impl ImageStore {
    pub fn new(limits: Limits, budget: ProcessBudget) -> Self {
        Self {
            limits,
            budget,
            used: 0,
            blobs: Vec::new(),
            by_hash: HashMap::new(),
            images: HashMap::new(),
            numbers: HashMap::new(),
            placements: Vec::new(),
            seq: 0,
            next_auto: AUTO_ID_BASE,
            next_auto_placement: AUTO_ID_BASE,
        }
    }

    /// Bytes charged to this pane.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Pane quota still free (for [`crate::kitty::MediaPolicy::quota_remaining`]).
    pub fn remaining(&self) -> u64 {
        self.limits.quota.saturating_sub(self.used)
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    pub fn get(&self, screen: Screen, id: ImageId) -> Option<Arc<Image>> {
        let e = self.images.get(&(screen, id))?;
        Some(self.blobs[e.blob].as_ref()?.image.clone())
    }

    /// The image and its blob serial (stable for these pixels for the life of the process).
    pub fn get_keyed(&self, screen: Screen, id: ImageId) -> Option<(u64, Arc<Image>)> {
        let e = self.images.get(&(screen, id))?;
        let b = self.blobs[e.blob].as_ref()?;
        Some((b.serial, b.image.clone()))
    }

    pub fn contains(&self, screen: Screen, id: ImageId) -> bool {
        self.images.contains_key(&(screen, id))
    }

    /// Reserve an auto id (upper half) before the image exists, e.g. while it decodes.
    pub fn next_auto_id(&mut self, screen: Screen) -> ImageId {
        self.alloc_id(screen)
    }

    /// Free an image, its placements and number mappings.
    pub fn free_image(&mut self, screen: Screen, id: ImageId) {
        self.remove_image(screen, id);
    }

    pub fn lookup_number(&self, screen: Screen, number: u32) -> Option<ImageId> {
        self.numbers.get(&(screen, number)).copied()
    }

    fn tick(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn alloc_id(&mut self, screen: Screen) -> ImageId {
        loop {
            let id = self.next_auto;
            self.next_auto = if id == u32::MAX { AUTO_ID_BASE } else { id + 1 };
            if !self.images.contains_key(&(screen, id)) {
                return id;
            }
        }
    }

    fn find_blob(&self, hash: u64, img: &Image) -> Option<usize> {
        self.by_hash
            .get(&hash)?
            .iter()
            .copied()
            .find(|&i| self.blobs[i].as_ref().is_some_and(|b| *b.image == *img))
    }

    /// One pass over the placements: which images have any, and which have a visible one.
    fn placement_index(&self) -> HashMap<(Screen, ImageId), bool> {
        let mut m: HashMap<(Screen, ImageId), bool> = HashMap::new();
        for p in &self.placements {
            let v = m.entry((p.screen, p.image)).or_insert(false);
            *v |= p.visible;
        }
        m
    }

    /// Store an image. `id == 0` auto-assigns from the upper half; `number` (`I=`) maps a
    /// client number to the id. An existing id is replaced (its placements are removed).
    pub fn insert(
        &mut self,
        screen: Screen,
        id: ImageId,
        number: u32,
        image: Image,
    ) -> Result<Inserted, StoreError> {
        let size = image.bytes();
        if size > self.limits.image_cap() {
            return Err(StoreError::TooLarge);
        }
        let hash = hash_image(&image);
        let shared = self.find_blob(hash, &image);
        let needed = if shared.is_some() { 0 } else { size };
        let id = if id == 0 { self.alloc_id(screen) } else { id };
        // Pin the shared blob so replacing/evicting its other holders cannot free it.
        if let Some(b) = shared
            && let Some(bl) = self.blobs[b].as_mut()
        {
            bl.refs += 1;
        }

        // Bytes freed by replacing the old image of this id.
        let old = self.images.get(&(screen, id)).map(|e| e.blob);
        let reclaim = match old {
            Some(b) if Some(b) != shared => self.blobs[b]
                .as_ref()
                .filter(|bl| bl.refs == 1)
                .map_or(0, |bl| bl.image.bytes()),
            _ => 0,
        };
        let pane_deficit = (self.used + needed)
            .saturating_sub(reclaim)
            .saturating_sub(self.limits.quota);
        let proc_deficit = (self.budget.used() + needed)
            .saturating_sub(reclaim)
            .saturating_sub(self.limits.quota_total);
        let deficit = pane_deficit.max(proc_deficit);

        let mut evicted = Vec::new();
        if deficit > 0 {
            let plan = self.plan_eviction(deficit, (screen, id), shared);
            let Some(plan) = plan else {
                if let Some(b) = shared
                    && let Some(bl) = self.blobs[b].as_mut()
                {
                    bl.refs -= 1;
                }
                return Err(StoreError::NoSpace);
            };
            for key in plan {
                self.remove_image(key.0, key.1);
                evicted.push(key);
            }
        }
        if old.is_some() {
            self.remove_image(screen, id);
        }
        let blob = match shared {
            Some(b) => b,
            None => {
                let b = self.new_blob(image, hash, size);
                if let Some(bl) = self.blobs[b].as_mut() {
                    bl.refs += 1;
                }
                b
            }
        };
        let last_use = self.tick();
        self.images.insert((screen, id), Entry { blob, last_use });
        if number != 0 {
            self.numbers.insert((screen, number), id);
        }
        Ok(Inserted { id, evicted })
    }

    fn new_blob(&mut self, image: Image, hash: u64, size: u64) -> usize {
        self.used += size;
        self.budget.0.fetch_add(size, Ordering::Relaxed);
        let blob = Blob {
            image: Arc::new(image),
            hash,
            refs: 0,
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
        };
        let idx = match self.blobs.iter().position(Option::is_none) {
            Some(i) => {
                self.blobs[i] = Some(blob);
                i
            }
            None => {
                self.blobs.push(Some(blob));
                self.blobs.len() - 1
            }
        };
        self.by_hash.entry(hash).or_default().push(idx);
        idx
    }

    /// Choose images to evict, tier 1 (unreferenced) then tier 2 (placements all off
    /// screen), each oldest first. `None` when even that cannot cover `deficit`.
    fn plan_eviction(
        &self,
        deficit: u64,
        protect: (Screen, ImageId),
        keep_blob: Option<usize>,
    ) -> Option<Vec<(Screen, ImageId)>> {
        let mut t1 = Vec::new();
        let mut t2 = Vec::new();
        let idx = self.placement_index();
        for (&(s, id), e) in &self.images {
            if (s, id) == protect {
                continue;
            }
            match idx.get(&(s, id)) {
                None => t1.push((e.last_use, s, id)),
                Some(false) => t2.push((e.last_use, s, id)),
                Some(true) => {}
            }
        }
        t1.sort_unstable();
        t2.sort_unstable();
        let mut refs: HashMap<usize, usize> = HashMap::new();
        let mut freed = 0u64;
        let mut plan = Vec::new();
        for (_, s, id) in t1.into_iter().chain(t2) {
            let b = self.images[&(s, id)].blob;
            let r = refs
                .entry(b)
                .or_insert_with(|| self.blobs[b].as_ref().map_or(0, |bl| bl.refs));
            *r = r.saturating_sub(1);
            if *r == 0 && Some(b) != keep_blob {
                freed += self.blobs[b].as_ref().map_or(0, |bl| bl.image.bytes());
            }
            plan.push((s, id));
            if freed >= deficit {
                return Some(plan);
            }
        }
        None
    }

    /// Remove an image, its placements and number mappings; free its blob if last user.
    fn remove_image(&mut self, screen: Screen, id: ImageId) {
        self.placements
            .retain(|p| !(p.screen == screen && p.image == id));
        self.numbers.retain(|&(s, _), v| !(s == screen && *v == id));
        let Some(e) = self.images.remove(&(screen, id)) else {
            return;
        };
        let mut free = None;
        if let Some(b) = self.blobs[e.blob].as_mut() {
            b.refs = b.refs.saturating_sub(1);
            if b.refs == 0 {
                free = Some((b.hash, b.image.bytes()));
            }
        }
        if let Some((hash, bytes)) = free {
            self.blobs[e.blob] = None;
            if let Some(v) = self.by_hash.get_mut(&hash) {
                v.retain(|&i| i != e.blob);
                if v.is_empty() {
                    self.by_hash.remove(&hash);
                }
            }
            self.used = self.used.saturating_sub(bytes);
            let _ = self
                .budget
                .0
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                    Some(u.saturating_sub(bytes))
                });
        }
    }

    /// Add or replace a placement of `image`. Returns the placement id (auto-assigned
    /// from the upper half when `pid == 0`).
    pub fn place(
        &mut self,
        screen: Screen,
        image: ImageId,
        pid: u32,
        spec: PlaceSpec,
    ) -> Result<u32, ErrCode> {
        if !self.images.contains_key(&(screen, image)) {
            return Err(ErrCode::Enoent);
        }
        let seq = self.tick();
        if let Some(e) = self.images.get_mut(&(screen, image)) {
            e.last_use = seq;
        }
        let pid = if pid == 0 {
            let p = self.next_auto_placement;
            self.next_auto_placement = if p == u32::MAX { AUTO_ID_BASE } else { p + 1 };
            p
        } else {
            pid
        };
        self.placements
            .retain(|p| !(p.screen == screen && p.image == image && p.id == pid));
        if self.placements.len() >= MAX_PLACEMENTS {
            self.placements.remove(0);
        }
        self.placements.push(Placement {
            screen,
            image,
            id: pid,
            spec,
            visible: true,
        });
        Ok(pid)
    }

    /// The grid reports whether a placement is on screen.
    pub fn set_visible(&mut self, screen: Screen, image: ImageId, pid: u32, visible: bool) {
        for p in &mut self.placements {
            if p.screen == screen && p.image == image && p.id == pid {
                p.visible = visible;
            }
        }
    }

    /// A placement's rows left history: drop it (the image stays, now evictable if unreferenced).
    pub fn drop_placement(&mut self, screen: Screen, image: ImageId, pid: u32) {
        self.placements
            .retain(|p| !(p.screen == screen && p.image == image && p.id == pid));
    }

    /// Apply a delete command. `positional` answers the cursor/cell/column/row selectors
    /// from the grid's geometry.
    pub fn delete(
        &mut self,
        screen: Screen,
        d: &DeleteSpec,
        positional: &dyn Fn(&Placement) -> bool,
    ) -> DeleteReport {
        let mut explicit: Vec<ImageId> = Vec::new();
        let hit = |p: &Placement, this: &Self| -> bool {
            if p.screen != screen {
                return false;
            }
            match d.target {
                DeleteTarget::Visible => p.visible,
                DeleteTarget::Id { id, placement } => {
                    p.image == id && (placement == 0 || p.id == placement)
                }
                DeleteTarget::Number { number, placement } => {
                    this.numbers.get(&(screen, number)) == Some(&p.image)
                        && (placement == 0 || p.id == placement)
                }
                DeleteTarget::Cursor
                | DeleteTarget::Cell { .. }
                | DeleteTarget::Column(_)
                | DeleteTarget::Row(_) => positional(p),
                DeleteTarget::CellZ { z, .. } => p.spec.z == z && positional(p),
                DeleteTarget::IdRange { lo, hi } => (lo..=hi).contains(&p.image),
                DeleteTarget::Z(z) => p.spec.z == z,
                DeleteTarget::Frames => false,
            }
        };
        match d.target {
            DeleteTarget::Id { id, .. } => explicit.push(id),
            DeleteTarget::Number { number, .. } => {
                explicit.extend(self.numbers.get(&(screen, number)).copied());
            }
            DeleteTarget::IdRange { lo, hi } => explicit.extend(
                self.images
                    .keys()
                    .filter(|(s, i)| *s == screen && (lo..=hi).contains(i))
                    .map(|k| k.1),
            ),
            _ => {}
        }
        let mut removed = 0;
        let mut touched: Vec<ImageId> = Vec::new();
        let old = std::mem::take(&mut self.placements);
        for p in old {
            if hit(&p, self) {
                removed += 1;
                touched.push(p.image);
            } else {
                self.placements.push(p);
            }
        }
        let mut freed = Vec::new();
        if d.free {
            touched.extend(explicit);
            touched.sort_unstable();
            touched.dedup();
            let idx = self.placement_index();
            for id in touched {
                // Explicit selectors with a placement id only free once no placement is left.
                if self.images.contains_key(&(screen, id)) && !idx.contains_key(&(screen, id)) {
                    self.remove_image(screen, id);
                    freed.push(id);
                }
            }
        }
        DeleteReport {
            placements_removed: removed,
            images_freed: freed,
        }
    }

    /// Remove every image of a partition (`graphics-clear`, reset, alt-screen exit).
    pub fn clear(&mut self, screen: Screen) {
        let ids: Vec<ImageId> = self
            .images
            .keys()
            .filter(|k| k.0 == screen)
            .map(|k| k.1)
            .collect();
        for id in ids {
            self.remove_image(screen, id);
        }
    }

    /// Alt-screen exit: its images vanish and never reach history.
    pub fn exit_alt(&mut self) {
        self.clear(Screen::Alt);
    }
}

impl Drop for ImageStore {
    fn drop(&mut self) {
        let used = self.used;
        let _ = self
            .budget
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(used))
            });
    }
}
