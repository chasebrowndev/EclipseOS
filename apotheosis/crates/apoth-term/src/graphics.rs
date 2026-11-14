// SPDX-License-Identifier: AGPL-3.0-only
//! Images in the grid (T-15 §1, §2, §4, §7): the kitty graphics APC and sixel DCS wired to
//! `apoth-image`, with placements anchored to tracked grid pins so they scroll, reflow and
//! clear with the text (GRAPHICS-01..18, -21, -23, -33, -34, -39..-44).
//!
//! Ordering. A kitty transmission is decoded off the io thread (T-15 §5). While one is
//! outstanding the terminal is *blocked*: `Term::feed` stops at the end of that APC and
//! holds the rest of the stream until [`crate::Term::image_done`] answers, so every later
//! byte (a placement of the new id, text the program prints after it) sees exactly the
//! state a synchronous decoder would give. The same barrier covers the consent for file
//! media (T-18 §3 `ask`). With [`GraphicsConfig::inline_decode`] (tests, fixtures, fuzzing)
//! the decode runs inside `feed` and nothing ever blocks.
//!
//! Sixel is a streaming state machine with byte and pixel caps; it decodes as the DCS
//! arrives (the T-15 §5 worker rule names PNG/JPEG/GIF/WebP decoding).
//!
//! Spec gaps decided here (reported): `U=1` (Unicode placeholders) and relative placements
//! answer `EINVAL` until M32; text overwriting a sixel slice removes that slice's whole
//! row segment (row granularity); when the cursor would leave the screen after a
//! placement its column is clamped to the last column with a pending wrap.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use apoth_grid::{PinId, PinKind};
use apoth_image::kitty::{
    Assembler, Command, DecodeDone, DecodeJob, DeleteTarget, Event, Header, Ids, MediaPolicy,
    Medium, Outcome, PlaceSpec, TransmitSpec, format_reply, parse_apc,
};
use apoth_image::sixel::{SixelDecoder, SixelParams};
use apoth_image::store::{AUTO_ID_BASE, ImageStore, ProcessBudget, Screen as Part};
use apoth_image::{ErrCode, Image, Limits};
use apoth_vt::{Params, StrEnd};

use crate::config::deny;
use crate::machine::{Machine, StrMode};

/// Largest cell box a placement may ask for (`c=`, `r=`).
const MAX_BOX: u32 = 10_000;
/// Header room on top of the base64 chunk cap for one APC.
const APC_HEAD_ROOM: usize = 4096;

/// Graphics configuration of one pane (the `graphics.*` keys, T-15 §4).
#[derive(Clone, Debug)]
pub struct GraphicsConfig {
    /// Caps and quotas.
    pub limits: Limits,
    /// The per-process budget every pane's store shares (`graphics.quota-total`).
    pub budget: ProcessBudget,
    /// `graphics.kitty`.
    pub kitty: bool,
    /// `graphics.sixel` (also DA1 attribute 4).
    pub sixel: bool,
    /// Decode inside `feed` instead of handing jobs to the caller. The binary sets this
    /// false and runs [`crate::Term::take_image_jobs`] on its worker pool.
    pub inline_decode: bool,
}

impl Default for GraphicsConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            budget: ProcessBudget::new(),
            kitty: true,
            sixel: true,
            inline_decode: true,
        }
    }
}

/// One image quad to draw, in physical pixels relative to the viewport's top-left cell.
#[derive(Clone, Debug)]
pub struct ImageDraw {
    /// Stable texture key for these pixels (process-unique blob serial).
    pub key: u64,
    pub image: Arc<Image>,
    /// x, y, w, h.
    pub dst: [i32; 4],
    /// Source rect in image pixels: x, y, w, h.
    pub src: [u32; 4],
    /// z-index: negative draws under the text.
    pub z: i32,
}

/// One placement as the grid sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementInfo {
    /// Image id (auto ids are in the upper half).
    pub image: u32,
    /// Placement id.
    pub placement: u32,
    /// Active-area row of the top-left cell (negative: in history).
    pub row: i64,
    pub col: usize,
    pub cols: u32,
    pub rows: u32,
    pub z: i32,
    pub sixel: bool,
}

#[derive(Clone, Copy, Debug)]
struct Geom {
    src: [u32; 4],
    /// Relative to the anchor cell's top-left, at cell size `cell`.
    dst: [i32; 4],
    cols: u32,
    rows: u32,
    cell: (u32, u32),
}

#[derive(Debug)]
struct Place {
    part: Part,
    image: u32,
    pid: u32,
    pin: PinId,
    geom: Geom,
    z: i32,
    sixel: bool,
    seq: u64,
}

#[derive(Debug)]
enum Op {
    /// Waiting for a worker.
    Decode(Pending),
    /// Waiting for the user's answer on file media.
    Ask(Pending, Vec<u8>),
}

#[derive(Debug)]
struct Pending {
    token: u64,
    /// Reply ids (with the assigned id when the request used a number).
    reply: Ids,
    quiet: u8,
    spec: TransmitSpec,
    place: Option<PlaceSpec>,
    query: bool,
    part: Part,
    id: u32,
    number: u32,
}

struct SixelRun {
    dec: SixelDecoder,
    ok: bool,
}

impl std::fmt::Debug for SixelRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SixelRun").field("ok", &self.ok).finish()
    }
}

/// Per-pane graphics state owned by the machine.
#[derive(Debug)]
pub(crate) struct Graphics {
    asm: Assembler,
    store: ImageStore,
    places: Vec<Place>,
    op: Option<Op>,
    jobs: Vec<DecodeJob>,
    next_token: u64,
    next_pid: u32,
    seq: u64,
    epoch: Option<Instant>,
    sixel: Option<SixelRun>,
    /// Shared registers while `?1070` is reset.
    sixel_regs: Option<Vec<[u8; 3]>>,
    /// XTSMGRAPHICS: colour registers and maximum geometry in force.
    sixel_reg_count: usize,
    sixel_geom: (u32, u32),
    /// Any sixel slice is placed (gates the text-overwrite checks).
    pub sixel_live: bool,
}

impl Graphics {
    pub(crate) fn new(cfg: &GraphicsConfig) -> Self {
        let l = &cfg.limits;
        Self {
            asm: Assembler::with_budget(l.clone(), cfg.budget.clone()),
            store: ImageStore::new(l.clone(), cfg.budget.clone()),
            places: Vec::new(),
            op: None,
            jobs: Vec::new(),
            next_token: 1,
            next_pid: AUTO_ID_BASE,
            seq: 0,
            epoch: None,
            sixel: None,
            sixel_regs: None,
            sixel_reg_count: l.sixel_max_registers,
            sixel_geom: (l.max_axis, l.max_axis),
            sixel_live: false,
        }
    }

    /// State carried across RIS: tokens stay unique and the clock keeps its epoch.
    pub(crate) fn carry_from(&mut self, old: &Graphics) {
        self.next_token = old.next_token;
        self.epoch = old.epoch;
    }

    /// A new string started while a sixel was open (it ended without `string_end`).
    pub(crate) fn sixel_abort(&mut self) {
        self.sixel = None;
    }

    pub(crate) fn blocked(&self) -> bool {
        self.op.is_some()
    }

    pub(crate) fn take_jobs(&mut self) -> Vec<DecodeJob> {
        std::mem::take(&mut self.jobs)
    }

    pub(crate) fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub(crate) fn has_places(&self) -> bool {
        !self.places.is_empty()
    }

    fn ms(&mut self, now: Option<Instant>) -> u64 {
        let Some(now) = now else { return 0 };
        let e = *self.epoch.get_or_insert(now);
        now.saturating_duration_since(e).as_millis() as u64
    }

    /// The chunk-stale deadline, if a chunked transfer is half received.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let ms = self.asm.next_expiry()?;
        Some(self.epoch? + std::time::Duration::from_millis(ms))
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        let ms = self.ms(Some(now));
        self.asm.expire(ms);
    }

    pub(crate) fn store(&self) -> &ImageStore {
        &self.store
    }
}

fn part_of(cur: usize) -> Part {
    if cur == 1 { Part::Alt } else { Part::Main }
}

fn grid_of(part: Part) -> usize {
    match part {
        Part::Main => 0,
        Part::Alt => 1,
    }
}

fn div_ceil(a: u64, b: u64) -> u64 {
    a.div_ceil(b.max(1))
}

fn clamp_i32(v: u64) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// Placement geometry (kitty "Controlling displayed image layout"): natural size is one
/// image pixel per physical pixel; one of `c`/`r` derives the other from the aspect ratio;
/// both letterbox/pillarbox inside the box. `X`/`Y` shift the origin within the first cell
/// and are not added to the cell count.
fn geom(iw: u32, ih: u32, s: &PlaceSpec, cell: (u32, u32)) -> Result<Geom, ErrCode> {
    let (cw, ch) = (u64::from(cell.0.max(1)), u64::from(cell.1.max(1)));
    if s.src_x >= iw || s.src_y >= ih {
        return Err(ErrCode::Einval);
    }
    let sw = if s.src_w == 0 {
        iw - s.src_x
    } else {
        s.src_w.min(iw - s.src_x)
    };
    let sh = if s.src_h == 0 {
        ih - s.src_y
    } else {
        s.src_h.min(ih - s.src_y)
    };
    let (swu, shu) = (u64::from(sw), u64::from(sh));
    let xo = u64::from(s.x_off).min(cw - 1);
    let yo = u64::from(s.y_off).min(ch - 1);
    let (c, r) = (s.cols.min(MAX_BOX), s.rows.min(MAX_BOX));
    let (px, py, dw, dh, cols, rows) = match (c, r) {
        (0, 0) => (0, 0, swu, shu, div_ceil(swu, cw), div_ceil(shu, ch)),
        (c, 0) => {
            let dw = u64::from(c) * cw;
            let dh = (dw * shu / swu).max(1);
            (0, 0, dw, dh, u64::from(c), div_ceil(dh, ch))
        }
        (0, r) => {
            let dh = u64::from(r) * ch;
            let dw = (dh * swu / shu).max(1);
            (0, 0, dw, dh, div_ceil(dw, cw), u64::from(r))
        }
        (c, r) => {
            let (bw, bh) = (u64::from(c) * cw, u64::from(r) * ch);
            // Fit: the limiting axis fills the box, the other is centred.
            let (dw, dh) = if bw * shu <= bh * swu {
                (bw, (bw * shu / swu).max(1))
            } else {
                ((bh * swu / shu).max(1), bh)
            };
            (
                (bw - dw) / 2,
                (bh - dh) / 2,
                dw,
                dh,
                u64::from(c),
                u64::from(r),
            )
        }
    };
    Ok(Geom {
        src: [s.src_x, s.src_y, sw, sh],
        dst: [
            clamp_i32(xo + px),
            clamp_i32(yo + py),
            clamp_i32(dw),
            clamp_i32(dh),
        ],
        cols: u32::try_from(cols.clamp(1, u64::from(MAX_BOX))).unwrap_or(MAX_BOX),
        rows: u32::try_from(rows.clamp(1, u64::from(MAX_BOX))).unwrap_or(MAX_BOX),
        cell,
    })
}

impl Machine {
    // ---- routing ------------------------------------------------------------------------

    /// `ESC _ G`: the APC route, or skip when kitty graphics are off or denied.
    pub(crate) fn gfx_apc_route(&mut self) -> Option<u32> {
        if !self.cfg.graphics.kitty || self.cfg.deny & deny::GRAPHICS_DIRECT != 0 {
            return None;
        }
        self.strm = StrMode::Kitty;
        let cap = self.cfg.graphics.limits.max_chunk_b64 + APC_HEAD_ROOM;
        Some(u32::try_from(cap).unwrap_or(u32::MAX))
    }

    /// `DCS P1;P2;P3 q`: start a sixel decode.
    pub(crate) fn gfx_sixel_start(&mut self, params: &Params) -> Option<u32> {
        if !self.cfg.graphics.sixel || self.cfg.deny & deny::GRAPHICS_DIRECT != 0 {
            return None;
        }
        let ps: Vec<u16> = params
            .values()
            .iter()
            .take(3)
            .map(|&v| u16::try_from(v).unwrap_or(u16::MAX))
            .collect();
        let bg = [self.bg.0, self.bg.1, self.bg.2, 255];
        let shared = !self.mode_on(false, 1070);
        let regs = if shared {
            self.gfx.sixel_regs.clone()
        } else {
            None
        };
        let mut limits = self.cfg.graphics.limits.clone();
        limits.sixel_max_registers = self.gfx.sixel_reg_count;
        let dec = SixelDecoder::new(SixelParams::from_dcs(&ps, bg, regs), &limits);
        self.gfx.sixel = Some(SixelRun { dec, ok: true });
        self.strm = StrMode::Sixel;
        Some(u32::try_from(limits.sixel_max_bytes).unwrap_or(u32::MAX))
    }

    pub(crate) fn gfx_sixel_put(&mut self, chunk: &[u8]) {
        if let Some(run) = self.gfx.sixel.as_mut()
            && run.ok
            && run.dec.feed(chunk).is_err()
        {
            run.ok = false;
        }
    }

    pub(crate) fn gfx_sixel_end(&mut self, end: StrEnd) {
        let Some(run) = self.gfx.sixel.take() else {
            return;
        };
        if !run.ok || !end.is_ok() {
            return;
        }
        let Ok(out) = run.dec.finish() else {
            return;
        };
        if !self.mode_on(false, 1070) {
            self.gfx.sixel_regs = Some(out.registers);
        }
        self.sixel_place(out.image);
    }

    /// The end of a kitty APC (`buf` starts at `G`).
    pub(crate) fn gfx_apc_end(&mut self, buf: &[u8], end: StrEnd) {
        if end == StrEnd::OverCap {
            if let Ok((h, _)) = parse_apc(buf) {
                self.gfx_reply(h.ids, Err(ErrCode::Efbig), h.quiet);
            }
            return;
        }
        if !end.is_ok() {
            return;
        }
        let now = self.gfx.ms(self.now);
        match self.gfx.asm.push(buf, now) {
            Event::Pending => {}
            Event::Error(f) => self.gfx_reply(f.ids, Err(f.code), f.quiet),
            Event::Command(h) => self.kitty_command(h),
            Event::Transfer(t) => self.kitty_transfer(t.header, t.payload),
        }
    }

    fn gfx_reply(&mut self, ids: Ids, res: Result<(), ErrCode>, quiet: u8) {
        if let Some(r) = format_reply(ids, res, quiet) {
            self.reply(r);
        }
    }

    // ---- kitty --------------------------------------------------------------------------

    fn kitty_command(&mut self, h: Header) {
        match &h.command {
            Command::Place(spec) => {
                let spec = spec.clone();
                let part = part_of(self.cur);
                let id = if h.ids.id != 0 {
                    Some(h.ids.id)
                } else if h.ids.number != 0 {
                    self.gfx.store.lookup_number(part, h.ids.number)
                } else {
                    None
                };
                let res = match id {
                    None if h.ids.number == 0 => Err(ErrCode::Einval),
                    None => Err(ErrCode::Enoent),
                    Some(id) => self.place(part, id, h.ids.placement, &spec),
                };
                let mut reply = h.ids;
                if let Some(id) = id {
                    reply.id = id;
                }
                self.gfx_reply(reply, res, h.quiet);
            }
            Command::Delete(d) => {
                let d = *d;
                self.gfx.asm.abort();
                self.kitty_delete(d);
            }
            // Animation and composition are R2 (M32).
            _ => self.gfx_reply(h.ids, Err(ErrCode::Einval), h.quiet),
        }
    }

    fn kitty_transfer(&mut self, h: Header, payload: Vec<u8>) {
        let (spec, place, query) = match h.command {
            Command::Transmit(s) => (s, None, false),
            Command::TransmitPlace(s, p) => (s, Some(p), false),
            Command::Query(s) => (s, None, true),
            _ => {
                self.gfx_reply(h.ids, Err(ErrCode::Einval), h.quiet);
                return;
            }
        };
        if let Some(p) = &place
            && (p.unicode || p.relative.is_some())
        {
            self.gfx_reply(h.ids, Err(ErrCode::Einval), h.quiet);
            return;
        }
        let part = part_of(self.cur);
        let (id, mut reply) = (h.ids.id, h.ids);
        let id = if query {
            id
        } else if id == 0 {
            // A number, or no id at all: the terminal assigns one from the upper half.
            let a = self.gfx.store.next_auto_id(part);
            if h.ids.number != 0 {
                reply.id = a;
            }
            a
        } else {
            id
        };
        let token = self.gfx.next_token;
        self.gfx.next_token += 1;
        let pending = Pending {
            token,
            reply,
            quiet: h.quiet,
            spec,
            place,
            query,
            part,
            id,
            number: h.ids.number,
        };
        if pending.spec.medium != Medium::Direct {
            let d = self.cfg.deny;
            if d & deny::GRAPHICS_FILE != 0 {
                return; // denied: no effect, no reply (T-18 §11)
            }
            if d & deny::GRAPHICS_FILE_ASK != 0 {
                self.events.graphics_file = Some(token);
                self.gfx.op = Some(Op::Ask(pending, payload));
                return;
            }
        }
        self.gfx_submit(pending, payload);
    }

    fn gfx_submit(&mut self, p: Pending, payload: Vec<u8>) {
        let policy = (p.spec.medium != Medium::Direct)
            .then(|| MediaPolicy::local_allow(self.gfx.store.remaining()));
        let job = DecodeJob {
            token: p.token,
            spec: p.spec.clone(),
            payload,
            policy,
            limits: self.cfg.graphics.limits.clone(),
        };
        if self.cfg.graphics.inline_decode {
            let done = job.run();
            self.gfx_complete(p, done.outcome);
        } else {
            self.gfx.jobs.push(job);
            self.gfx.op = Some(Op::Decode(p));
        }
    }

    /// The user's answer for file media (`allow` false: no effect, no reply).
    pub(crate) fn gfx_decision(&mut self, token: u64, allow: bool) {
        let Some(Op::Ask(p, _)) = &self.gfx.op else {
            return;
        };
        if p.token != token {
            return;
        }
        let Some(Op::Ask(p, payload)) = self.gfx.op.take() else {
            return;
        };
        if allow {
            self.gfx_submit(p, payload);
        }
    }

    /// A worker finished; unknown tokens (a reset in between) are ignored.
    pub(crate) fn gfx_done(&mut self, done: DecodeDone) {
        let Some(Op::Decode(p)) = &self.gfx.op else {
            return;
        };
        if p.token != done.token {
            return;
        }
        let Some(Op::Decode(p)) = self.gfx.op.take() else {
            return;
        };
        self.gfx_complete(p, done.outcome);
    }

    fn gfx_complete(&mut self, p: Pending, outcome: Outcome) {
        let img = match outcome {
            Outcome::Denied => return,
            Outcome::Failed(e) => {
                self.gfx_reply(p.reply, Err(e), p.quiet);
                return;
            }
            Outcome::Image(img) => img,
        };
        if p.query {
            self.gfx_reply(p.reply, Ok(()), p.quiet);
            return;
        }
        // Retransmitting an id replaces the image and removes its placements.
        self.remove_places(|q| q.part == p.part && q.image == p.id);
        self.sync_visible();
        let res = match self.gfx.store.insert(p.part, p.id, p.number, img) {
            Err(e) => Err(e.code()),
            Ok(ins) => {
                for (s, id) in ins.evicted {
                    self.remove_places(|q| q.part == s && q.image == id);
                }
                match &p.place {
                    Some(spec) => self.place(p.part, p.id, p.reply.placement, spec),
                    None => Ok(()),
                }
            }
        };
        self.gfx_reply(p.reply, res, p.quiet);
    }

    /// Place image `id` at the cursor (`a=p`, or the display half of `a=T`).
    fn place(&mut self, part: Part, id: u32, pid: u32, spec: &PlaceSpec) -> Result<(), ErrCode> {
        if spec.unicode || spec.relative.is_some() {
            return Err(ErrCode::Einval);
        }
        let img = self.gfx.store.get(part, id).ok_or(ErrCode::Enoent)?;
        let g = geom(img.width, img.height, spec, self.cfg.cell_px)?;
        if pid != 0 {
            self.remove_places(|q| q.part == part && q.image == id && q.pid == pid);
        }
        let pid = if pid != 0 {
            pid
        } else {
            let p = self.gfx.next_pid;
            self.gfx.next_pid = if p == u32::MAX { AUTO_ID_BASE } else { p + 1 };
            p
        };
        self.anchor(part, id, pid, g, spec.z, false, spec.clone())?;
        if !spec.no_cursor_move {
            for _ in 0..g.rows {
                self.index();
            }
            self.cursor_right(g.cols as usize);
        }
        Ok(())
    }

    fn cursor_right(&mut self, n: usize) {
        self.cursor_moved();
        let x = self.x.saturating_add(n);
        if x >= self.cols {
            self.x = self.cols - 1;
            self.wrap = self.autowrap;
        } else {
            self.x = x;
            self.wrap = false;
        }
    }

    /// Track a pin at the cursor (or `(y, x)`) and record the placement in both lists.
    #[allow(clippy::too_many_arguments)]
    fn anchor_at(
        &mut self,
        part: Part,
        id: u32,
        pid: u32,
        g: Geom,
        z: i32,
        sixel: bool,
        spec: PlaceSpec,
        y: usize,
        x: usize,
    ) -> Result<(), ErrCode> {
        let gi = grid_of(part);
        let pin = self.panes[gi]
            .grid
            .track_active(y, x, PinKind::ImagePlacement)
            .ok_or(ErrCode::Einval)?;
        if let Err(e) = self.gfx.store.place(part, id, pid, spec) {
            self.panes[gi].grid.untrack_pin(pin);
            return Err(e);
        }
        self.gfx.seq += 1;
        let seq = self.gfx.seq;
        self.gfx.places.push(Place {
            part,
            image: id,
            pid,
            pin,
            geom: g,
            z,
            sixel,
            seq,
        });
        if self.gfx.places.len() > self.gfx.store.placements().len() {
            self.reconcile(); // the store dropped its oldest placement
        }
        if sixel {
            self.gfx.sixel_live = true;
        }
        self.panes[gi].grid.mark_full();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn anchor(
        &mut self,
        part: Part,
        id: u32,
        pid: u32,
        g: Geom,
        z: i32,
        sixel: bool,
        spec: PlaceSpec,
    ) -> Result<(), ErrCode> {
        let (y, x) = (self.y, self.x);
        self.anchor_at(part, id, pid, g, z, sixel, spec, y, x)
    }

    /// Drop our placements the store no longer has.
    fn reconcile(&mut self) {
        let live: HashSet<(Part, u32, u32)> = self
            .gfx
            .store
            .placements()
            .iter()
            .map(|p| (p.screen, p.image, p.id))
            .collect();
        self.remove_places(|q| !live.contains(&(q.part, q.image, q.pid)));
    }

    /// Remove placements matching `f`: untrack their pins, drop them from the store, and
    /// free sixel images left without a slice.
    fn remove_places(&mut self, f: impl Fn(&Place) -> bool) {
        if !self.gfx.places.iter().any(&f) {
            return;
        }
        let old = std::mem::take(&mut self.gfx.places);
        let mut freed: Vec<(Part, u32)> = Vec::new();
        for p in old {
            if f(&p) {
                let gi = grid_of(p.part);
                self.panes[gi].grid.untrack_pin(p.pin);
                self.panes[gi].grid.mark_full();
                self.gfx.store.drop_placement(p.part, p.image, p.pid);
                if p.sixel {
                    freed.push((p.part, p.image));
                }
            } else {
                self.gfx.places.push(p);
            }
        }
        for (part, id) in freed {
            if !self
                .gfx
                .places
                .iter()
                .any(|q| q.part == part && q.image == id)
            {
                self.gfx.store.free_image(part, id);
            }
        }
        self.gfx.sixel_live = self.gfx.places.iter().any(|q| q.sixel);
    }

    /// Active-area cell box of a placement: (row, col, rows, cols); the row is negative
    /// while the top is in history.
    fn place_box(&self, p: &Place) -> Option<(i64, usize, u32, u32)> {
        let g = &self.panes[grid_of(p.part)].grid;
        let (abs, col) = g.pin_pos(p.pin)?;
        let row = abs as i64 - g.history_rows() as i64;
        Some((row, col, p.geom.rows, p.geom.cols))
    }

    fn intersects(&self, p: &Place, y: i64, x0: usize, x1: usize) -> bool {
        self.place_box(p).is_some_and(|(r, c, rows, cols)| {
            y >= r && y < r + i64::from(rows) && x0 < c + cols as usize && x1 > c
        })
    }

    /// Tell the store which placements are on screen (eviction and `d=a` use it).
    fn sync_visible(&mut self) {
        let cur = part_of(self.cur);
        let rows = self.rows as i64;
        let vis: Vec<(Part, u32, u32, bool)> = self
            .gfx
            .places
            .iter()
            .map(|p| {
                let on = p.part == cur
                    && self
                        .place_box(p)
                        .is_some_and(|(r, _, h, _)| r < rows && r + i64::from(h) > 0);
                (p.part, p.image, p.pid, on)
            })
            .collect();
        for (part, image, pid, on) in vis {
            self.gfx.store.set_visible(part, image, pid, on);
        }
    }

    fn kitty_delete(&mut self, d: apoth_image::kitty::DeleteSpec) {
        let part = part_of(self.cur);
        self.sync_visible();
        // Positional selectors are answered from the grid before the store runs.
        let (cy, cx) = (self.y as i64, self.x);
        let pos: HashSet<(u32, u32)> = self
            .gfx
            .places
            .iter()
            .filter(|p| p.part == part)
            .filter(|p| {
                let Some((r, c, rows, cols)) = self.place_box(p) else {
                    return false;
                };
                let in_row = |y: i64| y >= r && y < r + i64::from(rows);
                let in_col = |x: usize| x >= c && x < c + cols as usize;
                match d.target {
                    DeleteTarget::Cursor => in_row(cy) && in_col(cx),
                    DeleteTarget::Cell { x, y } | DeleteTarget::CellZ { x, y, .. } => {
                        in_row(i64::from(y) - 1) && in_col((x as usize).wrapping_sub(1))
                    }
                    DeleteTarget::Column(x) => in_col((x as usize).wrapping_sub(1)),
                    DeleteTarget::Row(y) => in_row(i64::from(y) - 1),
                    _ => false,
                }
            })
            .map(|p| (p.image, p.pid))
            .collect();
        self.gfx
            .store
            .delete(part, &d, &|p| pos.contains(&(p.image, p.id)));
        self.reconcile();
    }

    // ---- sixel --------------------------------------------------------------------------

    fn sixel_place(&mut self, mut img: Image) {
        let (gw, gh) = self.gfx.sixel_geom;
        if img.width > gw || img.height > gh {
            img = crop(&img, gw.min(img.width), gh.min(img.height));
        }
        let part = part_of(self.cur);
        let cell = self.cfg.cell_px;
        let (cw, ch) = (cell.0.max(1), cell.1.max(1));
        let slices = img.cell_row_slices(ch);
        let w = img.width;
        let cols = w.div_ceil(cw).max(1);
        let id = self.gfx.store.next_auto_id(part);
        self.sync_visible();
        let Ok(ins) = self.gfx.store.insert(part, id, 0, img) else {
            return;
        };
        for (s, eid) in ins.evicted {
            self.remove_places(|q| q.part == s && q.image == eid);
        }
        let scrolling = !self.mode_on(false, 80);
        let x0 = if scrolling { self.x } else { 0 };
        let mut y = if scrolling { self.y } else { 0 };
        let n = slices.len();
        for (i, r) in slices.into_iter().enumerate() {
            if i > 0 {
                if scrolling {
                    self.index();
                    y = self.y;
                } else {
                    y += 1;
                    if y >= self.rows {
                        break; // DECSDM: rows below the screen are clipped
                    }
                }
            }
            let h = r.end - r.start;
            let g = Geom {
                src: [0, r.start, w, h],
                dst: [0, 0, clamp_i32(u64::from(w)), clamp_i32(u64::from(h))],
                cols,
                rows: 1,
                cell,
            };
            let pid = self.gfx.next_pid;
            self.gfx.next_pid = if pid == u32::MAX {
                AUTO_ID_BASE
            } else {
                pid + 1
            };
            let spec = PlaceSpec {
                src_x: 0,
                src_y: r.start,
                src_w: w,
                src_h: h,
                cols,
                rows: 1,
                x_off: 0,
                y_off: 0,
                z: 0,
                no_cursor_move: true,
                unicode: false,
                relative: None,
            };
            let _ = self.anchor_at(part, id, pid, g, 0, true, spec, y, x0);
        }
        if !self
            .gfx
            .places
            .iter()
            .any(|q| q.part == part && q.image == id)
        {
            self.gfx.store.free_image(part, id);
        }
        if scrolling && n > 0 {
            self.cursor_moved();
            if self.mode_on(false, 8452) {
                self.x = x0;
                self.wrap = false;
                self.cursor_right(cols as usize);
            } else {
                self.index();
                self.x = x0;
                self.wrap = false;
            }
        }
    }

    /// Text written to (or erased from) active row `y`, columns `x0..x1`, removes sixel
    /// slices there (sixel is cell content; kitty images are not affected by text).
    #[inline]
    pub(crate) fn gfx_text_over(&mut self, y: usize, x0: usize, x1: usize) {
        if self.gfx.sixel_live {
            self.sixel_text_over(y, x0, x1);
        }
    }

    #[cold]
    fn sixel_text_over(&mut self, y: usize, x0: usize, x1: usize) {
        let part = part_of(self.cur);
        let hit: HashSet<u64> = self
            .gfx
            .places
            .iter()
            .filter(|p| p.sixel && p.part == part && self.intersects(p, y as i64, x0, x1))
            .map(|p| p.seq)
            .collect();
        if !hit.is_empty() {
            self.remove_places(|q| hit.contains(&q.seq));
        }
    }

    /// `ED 2`: every placement in the active area of the current screen goes.
    pub(crate) fn gfx_erase_screen(&mut self) {
        let part = part_of(self.cur);
        let rows = self.rows as i64;
        let hit: HashSet<u64> = self
            .gfx
            .places
            .iter()
            .filter(|p| {
                p.part == part
                    && self
                        .place_box(p)
                        .is_some_and(|(r, _, h, _)| r < rows && r + i64::from(h) > 0)
            })
            .map(|p| p.seq)
            .collect();
        if !hit.is_empty() {
            self.remove_places(|q| hit.contains(&q.seq));
        }
    }

    /// Alternate-screen images vanish on entry and exit and never reach history.
    pub(crate) fn gfx_clear_alt(&mut self) {
        self.remove_places(|q| q.part == Part::Alt);
        self.gfx.store.clear(Part::Alt);
    }

    /// Before `ED 3` replaces the primary screen: placements whose top row is in the active
    /// area, as (index, row, col); the rest are dropped with the history.
    pub(crate) fn gfx_before_history_clear(&mut self) -> Vec<(u64, usize, usize)> {
        let hist: HashSet<u64> = self
            .gfx
            .places
            .iter()
            .filter(|p| p.part == Part::Main)
            .filter(|p| self.place_box(p).is_none_or(|(r, ..)| r < 0))
            .map(|p| p.seq)
            .collect();
        self.remove_places(|q| hit_seq(&hist, q));
        self.gfx
            .places
            .iter()
            .filter(|p| p.part == Part::Main)
            .filter_map(|p| {
                let (r, c, ..) = self.place_box(p)?;
                Some((p.seq, r as usize, c))
            })
            .collect()
    }

    /// After `ED 3`: re-anchor the kept placements in the new primary screen.
    pub(crate) fn gfx_after_history_clear(&mut self, kept: Vec<(u64, usize, usize)>) {
        let mut lost = HashSet::new();
        for (seq, y, x) in kept {
            let pin =
                self.panes[0]
                    .grid
                    .track_active(y, x.min(self.cols - 1), PinKind::ImagePlacement);
            match (pin, self.gfx.places.iter_mut().find(|p| p.seq == seq)) {
                (Some(pin), Some(p)) => p.pin = pin,
                _ => {
                    lost.insert(seq);
                }
            }
        }
        if !lost.is_empty() {
            self.remove_places(|q| hit_seq(&lost, q));
        }
    }

    /// After each feed: placements whose rows left history (or a region scrolled them
    /// away) are dropped.
    pub(crate) fn gfx_collect(&mut self) {
        if self.gfx.places.is_empty() {
            // Still drain so the grids' garbage lists do not grow.
            let mut sink = Vec::new();
            self.panes[0].grid.take_garbage(&mut sink);
            self.panes[1].grid.take_garbage(&mut sink);
            return;
        }
        let mut dead = Vec::new();
        self.panes[0].grid.take_garbage(&mut dead);
        self.panes[1].grid.take_garbage(&mut dead);
        let dead: Vec<PinId> = dead
            .into_iter()
            .filter(|(_, k)| *k == PinKind::ImagePlacement)
            .map(|(p, _)| p)
            .collect();
        if !dead.is_empty() {
            self.remove_places(|q| dead.contains(&q.pin));
        }
    }

    /// XTSMGRAPHICS `CSI ? Pi ; Pa ; Pv S` (GRAPHICS-18).
    pub(crate) fn xtsmgraphics(&mut self, p: &Params) {
        if !self.cfg.graphics.sixel || self.cfg.deny & deny::GRAPHICS_DIRECT != 0 {
            return;
        }
        let l = &self.cfg.graphics.limits;
        let (max_regs, max_axis) = (l.sixel_max_registers, l.max_axis);
        let pi = p.get_or(0, 0);
        let pa = p.get_or(1, 0);
        let reply = match (pi, pa) {
            (1, 1) => format!("\x1b[?1;0;{}S", self.gfx.sixel_reg_count),
            (1, 2) => {
                self.gfx.sixel_reg_count = max_regs;
                format!("\x1b[?1;0;{max_regs}S")
            }
            (1, 3) => {
                let n = (p.get_or(2, 0) as usize).clamp(1, max_regs);
                self.gfx.sixel_reg_count = n;
                format!("\x1b[?1;0;{n}S")
            }
            (1, 4) => format!("\x1b[?1;0;{max_regs}S"),
            (2, 1) => {
                let (w, h) = self.sixel_geom_now();
                format!("\x1b[?2;0;{w};{h}S")
            }
            (2, 2) => {
                self.gfx.sixel_geom = (max_axis, max_axis);
                let (w, h) = self.sixel_geom_now();
                format!("\x1b[?2;0;{w};{h}S")
            }
            (2, 3) => {
                let w = p.get_or(2, 0).clamp(1, max_axis);
                let h = p.get_or(3, 0).clamp(1, max_axis);
                self.gfx.sixel_geom = (w, h);
                let (w, h) = self.sixel_geom_now();
                format!("\x1b[?2;0;{w};{h}S")
            }
            (2, 4) => format!("\x1b[?2;0;{max_axis};{max_axis}S"),
            (1 | 2, _) => format!("\x1b[?{pi};2;0S"),
            _ => format!("\x1b[?{pi};1;0S"),
        };
        self.reply_str(reply);
    }

    /// The geometry in force, bounded by the screen's pixel size.
    fn sixel_geom_now(&self) -> (u32, u32) {
        let (cw, ch) = self.cfg.cell_px;
        let sw = (self.cols as u32).saturating_mul(cw);
        let sh = (self.rows as u32).saturating_mul(ch);
        let (gw, gh) = self.gfx.sixel_geom;
        (gw.min(sw).max(1), gh.min(sh).max(1))
    }

    // ---- read side ----------------------------------------------------------------------

    /// Quads for the current screen with the viewport top at absolute row `top_abs`,
    /// `rows` rows tall, sorted bottom to top (z, then image id, then age).
    pub(crate) fn gfx_draws(&self, top_abs: usize, rows: usize, out: &mut Vec<ImageDraw>) {
        out.clear();
        let part = part_of(self.cur);
        let (cw, ch) = self.cfg.cell_px;
        let (cw, ch) = (i64::from(cw.max(1)), i64::from(ch.max(1)));
        let mut order: Vec<(&Place, i64, usize)> = Vec::new();
        for p in self.gfx.places.iter().filter(|p| p.part == part) {
            let g = &self.panes[grid_of(part)].grid;
            let Some((abs, col)) = g.pin_pos(p.pin) else {
                continue;
            };
            let vrow = abs as i64 - top_abs as i64;
            if vrow >= rows as i64 || vrow + i64::from(p.geom.rows) <= 0 {
                continue;
            }
            order.push((p, vrow, col));
        }
        order.sort_by_key(|(p, ..)| (p.z, p.image, p.seq));
        for (p, vrow, col) in order {
            let Some((key, image)) = self.gfx.store.get_keyed(p.part, p.image) else {
                continue;
            };
            let g = &p.geom;
            let (c0w, c0h) = (i64::from(g.cell.0.max(1)), i64::from(g.cell.1.max(1)));
            let sc = |v: i32, num: i64, den: i64| {
                let v = i64::from(v) * num;
                (v + den / 2).div_euclid(den)
            };
            let x = col as i64 * cw + sc(g.dst[0], cw, c0w);
            let y = vrow * ch + sc(g.dst[1], ch, c0h);
            let w = sc(g.dst[2], cw, c0w).max(1);
            let h = sc(g.dst[3], ch, c0h).max(1);
            let c = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
            out.push(ImageDraw {
                key,
                image,
                dst: [c(x), c(y), c(w), c(h)],
                src: g.src,
                z: p.z,
            });
        }
    }

    pub(crate) fn gfx_placements(&self) -> Vec<PlacementInfo> {
        let part = part_of(self.cur);
        let mut v: Vec<(u64, PlacementInfo)> = self
            .gfx
            .places
            .iter()
            .filter(|p| p.part == part)
            .filter_map(|p| {
                let (row, col, rows, cols) = self.place_box(p)?;
                Some((
                    p.seq,
                    PlacementInfo {
                        image: p.image,
                        placement: p.pid,
                        row,
                        col,
                        cols,
                        rows,
                        z: p.z,
                        sixel: p.sixel,
                    },
                ))
            })
            .collect();
        v.sort_by_key(|(s, _)| *s);
        v.into_iter().map(|(_, p)| p).collect()
    }
}

fn hit_seq(set: &HashSet<u64>, p: &Place) -> bool {
    set.contains(&p.seq)
}

/// Top-left `w` x `h` of `img`.
fn crop(img: &Image, w: u32, h: u32) -> Image {
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let at = y * img.width as usize * 4;
        rgba.extend_from_slice(&img.rgba[at..at + w as usize * 4]);
    }
    Image {
        width: w,
        height: h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PlaceSpec {
        PlaceSpec {
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            cols: 0,
            rows: 0,
            x_off: 0,
            y_off: 0,
            z: 0,
            no_cursor_move: false,
            unicode: false,
            relative: None,
        }
    }

    #[test]
    fn natural_size_rounds_cells_up_and_ignores_offsets() {
        let mut s = spec();
        s.x_off = 7;
        let g = geom(17, 16, &s, (8, 16)).unwrap();
        assert_eq!((g.cols, g.rows), (3, 1));
        assert_eq!(g.dst, [7, 0, 17, 16]);
    }

    #[test]
    fn one_side_derives_the_other_from_aspect() {
        let mut s = spec();
        s.cols = 4; // 32 px wide
        let g = geom(64, 32, &s, (8, 16)).unwrap();
        assert_eq!(g.dst, [0, 0, 32, 16]);
        assert_eq!((g.cols, g.rows), (4, 1));
    }

    #[test]
    fn both_sides_letterbox_centred() {
        let mut s = spec();
        s.cols = 4;
        s.rows = 4; // 32 x 64 box
        let g = geom(10, 10, &s, (8, 16)).unwrap();
        assert_eq!(g.dst, [0, 16, 32, 32]);
        assert_eq!((g.cols, g.rows), (4, 4));
    }

    #[test]
    fn source_rect_outside_is_einval_and_offsets_clamp_below_cell() {
        let mut s = spec();
        s.src_x = 10;
        assert_eq!(geom(10, 10, &s, (8, 16)).unwrap_err(), ErrCode::Einval);
        let mut s = spec();
        s.x_off = 100;
        s.y_off = 100;
        let g = geom(4, 4, &s, (8, 16)).unwrap();
        assert_eq!((g.dst[0], g.dst[1]), (7, 15));
    }
}
