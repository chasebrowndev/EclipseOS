// SPDX-License-Identifier: AGPL-3.0-only
//! Per-pane image store: one byte budget across kitty, sixel and iTerm2 images, tiered
//! eviction, content-hash sharing, main/alternate partitions (T-15 §4, GRAPHICS-13..15,
//! PERFORMANCE-51/52).
//!
//! Pins, tracked-pin anchoring and reflow are grid-side (M25 proper): the grid tells the
//! store which placements are on screen ([`ImageStore::set_visible`]) and answers the
//! positional delete selectors through a predicate.

use crate::anim::{Animation, Frame};
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
    /// Frames and playback, once the image has more than its root frame or was animated.
    anim: Option<Box<Animation>>,
    /// The pixels this id showed before a same-size retransmission: the renderer reuses
    /// that texture for the new pixels (a video stream retransmitting one id).
    replaces: Option<u64>,
}

/// What to draw for an image at an instant.
#[derive(Debug, Clone)]
pub struct FrameView {
    /// Texture key of these pixels.
    pub key: u64,
    pub image: Arc<Image>,
    /// A key whose texture may be overwritten with these pixels (same size)...
    pub replaces: Option<u64>,
    /// ...of which only this rect differs (`None`: all of it).
    pub dirty: Option<[u32; 4]>,
    /// When the shown frame changes next (pane clock, ms).
    pub due: Option<u64>,
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
    /// Bytes of animation frames (beyond each root), against `graphics.animation-quota`.
    anim_used: u64,
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
            anim_used: 0,
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
        let hash = image.content_hash();
        self.insert_hashed(screen, id, number, image, hash)
    }

    /// [`ImageStore::insert`] with the content hash already computed (on the worker that
    /// decoded the image, so the io thread never scans the pixels).
    pub fn insert_hashed(
        &mut self,
        screen: Screen,
        id: ImageId,
        number: u32,
        image: Image,
        hash: u64,
    ) -> Result<Inserted, StoreError> {
        let size = image.bytes();
        if size > self.limits.image_cap() {
            return Err(StoreError::TooLarge);
        }
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
        // Same-size retransmission: the renderer may reuse the old texture.
        let replaces = old.and_then(|b| {
            let bl = self.blobs[b].as_ref()?;
            (bl.image.width == image.width && bl.image.height == image.height).then_some(bl.serial)
        });
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
        self.images.insert(
            (screen, id),
            Entry {
                blob,
                last_use,
                anim: None,
                replaces,
            },
        );
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
        if let Some(a) = &e.anim {
            self.anim_used = self.anim_used.saturating_sub(a.bytes());
        }
        self.release_blob(e.blob);
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
                DeleteTarget::Frames { .. } => false,
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

    // ---- animation (T-15 §1 R2) -------------------------------------------------------------

    /// Bytes of animation frames charged to this pane.
    pub fn anim_used(&self) -> u64 {
        self.anim_used
    }

    fn anim_mut(&mut self, screen: Screen, id: ImageId) -> Option<&mut Animation> {
        let e = self.images.get(&(screen, id))?;
        if e.anim.is_none() {
            let b = self.blobs[e.blob].as_ref()?;
            let a = Box::new(Animation::new(b.image.clone(), b.serial));
            self.images.get_mut(&(screen, id))?.anim = Some(a);
        }
        self.images.get_mut(&(screen, id))?.anim.as_deref_mut()
    }

    /// Frames of an image (1 for a still image), or `None` when the image does not exist.
    pub fn frame_count(&self, screen: Screen, id: ImageId) -> Option<usize> {
        let e = self.images.get(&(screen, id))?;
        Some(e.anim.as_ref().map_or(1, |a| a.len()))
    }

    /// The pixels of frame `index` (0-based; 0 is the root).
    pub fn frame_image(&self, screen: Screen, id: ImageId, index: usize) -> Option<Arc<Image>> {
        let e = self.images.get(&(screen, id))?;
        match &e.anim {
            Some(a) => a.frame(index).map(|f| f.image.clone()),
            None if index == 0 => Some(self.blobs[e.blob].as_ref()?.image.clone()),
            None => None,
        }
    }

    /// The animation state of an image (tests, diagnostics).
    pub fn animation(&self, screen: Screen, id: ImageId) -> Option<&Animation> {
        self.images.get(&(screen, id))?.anim.as_deref()
    }

    /// Append a composed frame (`a=f` without `r`). Returns its 1-based number.
    pub fn add_frame(
        &mut self,
        screen: Screen,
        id: ImageId,
        frame: Frame,
        now: u64,
    ) -> Result<u32, ErrCode> {
        let quota = self.limits.animation_quota;
        let used = self.anim_used;
        let a = self.anim_mut(screen, id).ok_or(ErrCode::Enoent)?;
        let before = a.bytes();
        // The pane quota is shared by every animation of the pane.
        let room = quota.saturating_sub(used.saturating_sub(before));
        let n = a.push(frame, room, now)?;
        let after = a.bytes();
        self.anim_used = used - before + after;
        Ok(n)
    }

    /// Replace frame `index` (0-based) with pixels that changed inside `rect`. Frame 0 is
    /// the root image: it gets a blob of its own (another id may share the old pixels).
    pub fn edit_frame(
        &mut self,
        screen: Screen,
        id: ImageId,
        index: usize,
        image: Image,
        rect: Option<[u32; 4]>,
        gap: Option<i32>,
    ) -> Result<(), ErrCode> {
        let count = self.frame_count(screen, id).ok_or(ErrCode::Enoent)?;
        if index >= count {
            return Err(ErrCode::Enoent);
        }
        if index == 0 {
            let size = image.bytes();
            let hash = image.content_hash();
            let old = self.images[&(screen, id)].blob;
            let old_refs = self.blobs[old].as_ref().map_or(0, |b| b.refs);
            let reclaim = if old_refs == 1 {
                self.blobs[old].as_ref().map_or(0, |b| b.image.bytes())
            } else {
                0
            };
            if (self.used + size).saturating_sub(reclaim) > self.limits.quota
                || (self.budget.used() + size).saturating_sub(reclaim) > self.limits.quota_total
            {
                return Err(ErrCode::Enospc);
            }
            let b = self.new_blob(image, hash, size);
            if let Some(bl) = self.blobs[b].as_mut() {
                bl.refs += 1;
            }
            let (img, serial) = {
                let bl = self.blobs[b].as_ref().ok_or(ErrCode::Enoent)?;
                (bl.image.clone(), bl.serial)
            };
            // Release the old blob through the ordinary path, keeping the entry.
            self.release_blob(old);
            if let Some(e) = self.images.get_mut(&(screen, id)) {
                e.blob = b;
            }
            let a = self.anim_mut(screen, id).ok_or(ErrCode::Enoent)?;
            a.replace(
                0,
                Frame {
                    image: img,
                    serial,
                    gap: 0,
                    delta: None,
                },
                rect,
            );
            if let Some(g) = gap
                && let Some(f) = self.anim_mut(screen, id).and_then(|a| a.frame_mut(0))
            {
                f.gap = g;
            }
            return Ok(());
        }
        let quota = self.limits.animation_quota;
        let used = self.anim_used;
        let a = self.anim_mut(screen, id).ok_or(ErrCode::Enoent)?;
        let before = a.bytes();
        let old_bytes = a.frame(index).map_or(0, |f| f.image.bytes());
        if used - old_bytes + image.bytes() > quota {
            return Err(ErrCode::Enospc);
        }
        a.replace(
            index,
            Frame {
                image: Arc::new(image),
                serial: crate::anim::frame_serial(),
                gap: 0,
                delta: None,
            },
            rect,
        );
        if let Some(g) = gap
            && let Some(f) = a.frame_mut(index)
        {
            f.gap = g;
        }
        let after = a.bytes();
        self.anim_used = used - before + after;
        Ok(())
    }

    /// `a=a` on an image.
    #[allow(clippy::too_many_arguments)]
    pub fn animate(
        &mut self,
        screen: Screen,
        id: ImageId,
        state: u32,
        loops: u32,
        edit: u32,
        gap: i32,
        current: u32,
        now: u64,
    ) -> Result<(), ErrCode> {
        let a = self.anim_mut(screen, id).ok_or(ErrCode::Enoent)?;
        a.control(state, loops, edit, gap, current, now)
    }

    /// `d=f`/`d=F`: drop the frames after the root of `id` (or of every image when `None`).
    pub fn clear_frames(&mut self, screen: Screen, id: Option<ImageId>) {
        let mut freed = 0;
        for (&(s, i), e) in &mut self.images {
            if s != screen || id.is_some_and(|x| x != i) {
                continue;
            }
            if let Some(a) = e.anim.as_mut() {
                freed += a.bytes();
                a.clear_frames();
            }
        }
        self.anim_used = self.anim_used.saturating_sub(freed);
    }

    /// What image `id` shows at `now` (pane clock, ms).
    pub fn view(&self, screen: Screen, id: ImageId, now: u64) -> Option<FrameView> {
        let e = self.images.get(&(screen, id))?;
        let b = self.blobs[e.blob].as_ref()?;
        let Some(a) = e.anim.as_ref().filter(|a| a.len() > 1) else {
            return Some(FrameView {
                key: b.serial,
                image: b.image.clone(),
                replaces: e.replaces,
                dirty: None,
                due: None,
            });
        };
        let p = a.at(now);
        let f = a.frame(p.index)?;
        let prev = a.frame((p.index + a.len() - 1) % a.len())?;
        Some(FrameView {
            key: f.serial,
            image: f.image.clone(),
            replaces: Some(prev.serial),
            dirty: if p.index == 0 { None } else { f.delta },
            due: p.due,
        })
    }

    /// Drop one reference to a blob; free it with the last.
    fn release_blob(&mut self, blob: usize) {
        let mut free = None;
        if let Some(b) = self.blobs[blob].as_mut() {
            b.refs = b.refs.saturating_sub(1);
            if b.refs == 0 {
                free = Some((b.hash, b.image.bytes()));
            }
        }
        if let Some((hash, bytes)) = free {
            self.blobs[blob] = None;
            if let Some(v) = self.by_hash.get_mut(&hash) {
                v.retain(|&i| i != blob);
                if v.is_empty() {
                    self.by_hash.remove(&hash);
                }
            }
            self.used = self.used.saturating_sub(bytes);
            self.budget.sub(bytes);
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
