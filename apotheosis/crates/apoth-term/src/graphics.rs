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
//! R2 (M32): animation (`a=f`, `a=a`, `a=c`; frames composed on the worker, `a=c` on the
//! io thread), Unicode placeholders (`U=1` virtual placements drawn where U+10EEEE cells
//! are, merged per run), relative placements (`P,Q,H,V`, depth 8, cycle check, cascade
//! delete) and iTerm2 `File=` images.
//!
//! Spec gaps decided here (reported): text overwriting a sixel slice removes that slice's
//! whole row segment (row granularity); when the cursor would leave the screen after a
//! placement its column is clamped to the last column with a pending wrap; `Q=0` names the
//! oldest placement of the parent image; a child of a virtual placement is positioned only
//! while drawing (positional deletes do not see it); an iTerm2 image leaves the cursor one
//! column right of its last row.
//!
//! Sessions (T-16 §1.1). `ESC _ M a=b,i=<n>` (the `apoth icat` begin marker) opens a
//! session: placements made while it is open belong to it. The caller binds it to the
//! foreground process group ([`crate::Term::bind_image_session`]); when that group leaves
//! the foreground without `a=e` ([`crate::Term::image_session_gone`]) every placement of
//! the session goes. `a=e` closes it cleanly: still images stay, animations go.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use apoth_grid::{Color, PinId, PinKind};
use apoth_image::anim::{DEFAULT_GAP_MS, composite, frame_serial, rect_inside};
use apoth_image::iterm::{ITermFile, cell_box, parse_file};
use apoth_image::kitty::{
    Assembler, Canvas, Command, DecodeDone, DecodeJob, DeleteTarget, Event, FrameJob, FrameSpec,
    Header, Ids, MediaPolicy, Medium, Outcome, PlaceSpec, Relative, TransmitSpec, Work,
    format_reply, parse_apc,
};
use apoth_image::placeholder::{self, Merger, Resolver};
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
/// Deepest relative placement chain (T-15 §1).
const MAX_DEPTH: usize = 8;

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
    /// `graphics.iterm2`: OSC 1337 `File=` inline images.
    pub iterm2: bool,
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
            iterm2: true,
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
    /// A key whose texture holds a same-size image these pixels may overwrite (the
    /// previous animation frame, or the image this id replaced)...
    pub replaces: Option<u64>,
    /// ...of which only this rect differs (`None`: all of it). RENDERING-37.
    pub dirty: Option<[u32; 4]>,
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
    /// The anchor cell; `None` for a relative placement (positioned from its parent).
    pin: Option<PinId>,
    geom: Geom,
    z: i32,
    sixel: bool,
    seq: u64,
    rel: Option<Relative>,
    /// Session that made it (0: none).
    session: u64,
}

/// A virtual placement (`U=1`): drawn wherever placeholder cells name it.
#[derive(Debug)]
struct Virt {
    part: Part,
    image: u32,
    pid: u32,
    spec: PlaceSpec,
    seq: u64,
    session: u64,
}

/// A virtual placement's drawn origin: (part, image, placement, absolute row, column).
type VirtOrigin = (Part, u32, u32, i64, i64);

/// An `apoth icat` session (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Session {
    id: u64,
    /// Foreground process group bound by the caller (0: not yet bound). Only for a
    /// session without a watched pid.
    pgid: i32,
    /// `p=`: the command's pid, which the caller watches for exit (0: none, or the caller
    /// could not watch it and fell back to the process group rule).
    pid: i32,
    /// The caller has taken `pid` to watch.
    watched: bool,
}

#[derive(Debug)]
enum Op {
    /// Waiting for a worker.
    Decode(Pending),
    /// Waiting for the user's answer on file media.
    Ask(Pending, Vec<u8>, Work),
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
    kind: PendKind,
}

#[derive(Debug, Clone)]
enum PendKind {
    Transmit,
    /// `a=f`: edit frame `index` (0-based) or append when `None`; gap to set.
    Frame {
        index: Option<usize>,
        gap: Option<i32>,
    },
    /// iTerm2 `File=`: cell box from these arguments.
    ITerm(ITermFile),
}

enum Parent {
    Anchored,
    Virt,
    Rel(Relative),
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
    virt: Vec<Virt>,
    sessions: Vec<Session>,
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
            virt: Vec::new(),
            sessions: Vec::new(),
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
        !self.places.is_empty() || !self.virt.is_empty()
    }

    /// Sessions opened but not yet bound to a process group (the caller binds them).
    pub(crate) fn unbound_sessions(&self) -> impl Iterator<Item = u64> + '_ {
        self.sessions
            .iter()
            .filter(|s| s.pgid == 0 && s.pid == 0)
            .map(|s| s.id)
    }

    /// Sessions with a `p=` pid nobody watches yet; marks them watched.
    pub(crate) fn take_session_pids(&mut self) -> Vec<(u64, i32)> {
        let mut out = Vec::new();
        for s in &mut self.sessions {
            if s.pid != 0 && !s.watched {
                s.watched = true;
                out.push((s.id, s.pid));
            }
        }
        out
    }

    /// The caller cannot watch session `id`'s pid: the process group rule applies.
    pub(crate) fn unwatch_session(&mut self, id: u64) {
        for s in &mut self.sessions {
            if s.id == id {
                s.pid = 0;
            }
        }
    }

    pub(crate) fn has_session(&self, id: u64) -> bool {
        self.sessions.iter().any(|s| s.id == id)
    }

    /// A session relies on the process group rule (the caller re-checks it on a timer).
    pub(crate) fn has_group_sessions(&self) -> bool {
        self.sessions.iter().any(|s| s.pid == 0)
    }

    pub(crate) fn sessions_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Bound sessions whose process group is not `fg`.
    pub(crate) fn sessions_not_in(&self, fg: i32) -> impl Iterator<Item = u64> + '_ {
        self.sessions
            .iter()
            .filter(move |s| s.pid == 0 && s.pgid != 0 && s.pgid != fg)
            .map(|s| s.id)
    }

    pub(crate) fn bind_session(&mut self, id: u64, pgid: i32) {
        for s in &mut self.sessions {
            if s.id == id && s.pgid == 0 {
                s.pgid = pgid;
            }
        }
    }

    fn session(&self) -> u64 {
        self.sessions.last().map_or(0, |s| s.id)
    }

    /// The pane clock (ms) at `now` without starting it.
    fn ms_at(&self, now: Instant) -> u64 {
        self.epoch
            .map_or(0, |e| now.saturating_duration_since(e).as_millis() as u64)
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

    /// The image an `i=`/`I=` pair names on screen `part`.
    fn named(&self, part: Part, ids: &Ids) -> Result<u32, ErrCode> {
        if ids.id != 0 {
            Ok(ids.id)
        } else if ids.number != 0 {
            self.gfx
                .store
                .lookup_number(part, ids.number)
                .ok_or(ErrCode::Enoent)
        } else {
            Err(ErrCode::Einval)
        }
    }

    fn kitty_command(&mut self, h: Header) {
        let part = part_of(self.cur);
        match &h.command {
            Command::Place(spec) => {
                let spec = spec.clone();
                let id = self.named(part, &h.ids);
                let res = id.and_then(|id| self.place(part, id, h.ids.placement, &spec));
                let mut reply = h.ids;
                if let Ok(id) = id {
                    reply.id = id;
                }
                self.gfx_reply(reply, res, h.quiet);
            }
            Command::Delete(d) => {
                let d = *d;
                self.gfx.asm.abort();
                self.kitty_delete(d);
            }
            Command::Animate(a) => {
                let a = *a;
                let now = self.gfx.ms(self.now);
                let res = self.named(part, &h.ids).and_then(|id| {
                    self.gfx.store.animate(
                        part,
                        id,
                        a.state,
                        a.loops,
                        a.edit_frame,
                        a.gap_ms,
                        a.current_frame,
                        now,
                    )
                });
                self.images_changed(part);
                self.gfx_reply(h.ids, res, h.quiet);
            }
            Command::Compose(c) => {
                let c = *c;
                let res = self
                    .named(part, &h.ids)
                    .and_then(|id| self.compose(part, id, &c));
                self.images_changed(part);
                self.gfx_reply(h.ids, res, h.quiet);
            }
            _ => self.gfx_reply(h.ids, Err(ErrCode::Einval), h.quiet),
        }
    }

    /// Pixels of a placed image changed: the next frame redraws.
    fn images_changed(&mut self, part: Part) {
        self.panes[grid_of(part)].grid.mark_full();
    }

    /// `a=c`: copy a rectangle of frame `r` onto frame `c`. Runs on the io thread: it is a
    /// bounded copy of pixels already decoded (no parsing), at most one frame's size.
    fn compose(
        &mut self,
        part: Part,
        id: u32,
        c: &apoth_image::kitty::ComposeSpec,
    ) -> Result<(), ErrCode> {
        let n = self
            .gfx
            .store
            .frame_count(part, id)
            .ok_or(ErrCode::Enoent)?;
        let (si, di) = (c.src_frame as usize, c.dst_frame as usize);
        if si == 0 || di == 0 || si > n || di > n {
            return Err(ErrCode::Enoent);
        }
        let src = self
            .gfx
            .store
            .frame_image(part, id, si - 1)
            .ok_or(ErrCode::Enoent)?;
        let dst = self
            .gfx
            .store
            .frame_image(part, id, di - 1)
            .ok_or(ErrCode::Enoent)?;
        let w = if c.w == 0 {
            src.width.saturating_sub(c.src_x)
        } else {
            c.w
        };
        let h = if c.h == 0 {
            src.height.saturating_sub(c.src_y)
        } else {
            c.h
        };
        if w == 0
            || h == 0
            || !rect_inside(c.src_x, c.src_y, w, h, src.width, src.height)
            || !rect_inside(c.dst_x, c.dst_y, w, h, dst.width, dst.height)
        {
            return Err(ErrCode::Einval);
        }
        if si == di {
            let ox = c.src_x < c.dst_x + w && c.dst_x < c.src_x + w;
            let oy = c.src_y < c.dst_y + h && c.dst_y < c.src_y + h;
            if ox && oy {
                return Err(ErrCode::Einval);
            }
        }
        let mut out = (*dst).clone();
        composite(
            &mut out, &src, c.src_x, c.src_y, c.dst_x, c.dst_y, w, h, c.replace,
        );
        self.gfx
            .store
            .edit_frame(part, id, di - 1, out, Some([c.dst_x, c.dst_y, w, h]), None)
    }

    fn kitty_transfer(&mut self, h: Header, payload: Vec<u8>) {
        let part = part_of(self.cur);
        let (spec, place, query, kind, work) = match h.command {
            Command::Transmit(s) => (s, None, false, PendKind::Transmit, Work::Kitty),
            Command::TransmitPlace(s, p) => (s, Some(p), false, PendKind::Transmit, Work::Kitty),
            Command::Query(s) => (s, None, true, PendKind::Transmit, Work::Kitty),
            Command::Frame(f) => match self.frame_job(part, &h.ids, &f) {
                Ok((kind, job)) => (f.transmit, None, false, kind, Work::Frame(job)),
                Err(e) => {
                    self.gfx_reply(h.ids, Err(e), h.quiet);
                    return;
                }
            },
            _ => {
                self.gfx_reply(h.ids, Err(ErrCode::Einval), h.quiet);
                return;
            }
        };
        let frame = matches!(kind, PendKind::Frame { .. });
        let (id, mut reply) = (h.ids.id, h.ids);
        let id = if query {
            id
        } else if frame {
            self.named(part, &h.ids).unwrap_or(0)
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
            number: if frame { 0 } else { h.ids.number },
            kind,
        };
        if pending.spec.medium != Medium::Direct {
            let d = self.cfg.deny;
            if d & deny::GRAPHICS_FILE != 0 {
                return; // denied: no effect, no reply (T-18 §11)
            }
            if d & deny::GRAPHICS_FILE_ASK != 0 {
                self.events.graphics_file = Some(token);
                self.gfx.op = Some(Op::Ask(pending, payload, work));
                return;
            }
        }
        self.gfx_submit(pending, payload, work);
    }

    /// The composition half of an `a=f` job: canvas, landing point, the frame it follows.
    fn frame_job(
        &self,
        part: Part,
        ids: &Ids,
        f: &FrameSpec,
    ) -> Result<(PendKind, FrameJob), ErrCode> {
        let id = self.named(part, ids)?;
        let n = self
            .gfx
            .store
            .frame_count(part, id)
            .ok_or(ErrCode::Enoent)?;
        let root = self
            .gfx
            .store
            .frame_image(part, id, 0)
            .ok_or(ErrCode::Enoent)?;
        let frame = |k: u32| -> Result<Arc<Image>, ErrCode> {
            if k == 0 || k as usize > n {
                return Err(ErrCode::Enoent);
            }
            self.gfx
                .store
                .frame_image(part, id, k as usize - 1)
                .ok_or(ErrCode::Enoent)
        };
        let overwrite = f.composition == 1;
        if f.edit_frame != 0 {
            let canvas = frame(f.edit_frame)?;
            return Ok((
                PendKind::Frame {
                    index: Some(f.edit_frame as usize - 1),
                    gap: f.gap_ms,
                },
                FrameJob {
                    canvas: Canvas::Frame(canvas),
                    x: f.x,
                    y: f.y,
                    overwrite,
                    prev: None,
                },
            ));
        }
        let canvas = if f.base_frame != 0 {
            Canvas::Frame(frame(f.base_frame)?)
        } else {
            Canvas::Fill {
                width: root.width,
                height: root.height,
                rgba: f.background.to_be_bytes(),
            }
        };
        let prev = frame(n as u32)?;
        Ok((
            PendKind::Frame {
                index: None,
                gap: f.gap_ms,
            },
            FrameJob {
                canvas,
                x: f.x,
                y: f.y,
                overwrite,
                prev: Some(prev),
            },
        ))
    }

    fn gfx_submit(&mut self, p: Pending, payload: Vec<u8>, work: Work) {
        let policy = (p.spec.medium != Medium::Direct)
            .then(|| MediaPolicy::local_allow(self.gfx.store.remaining()));
        let job = DecodeJob {
            token: p.token,
            spec: p.spec.clone(),
            payload,
            policy,
            limits: self.cfg.graphics.limits.clone(),
            work,
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
        let Some(Op::Ask(p, ..)) = &self.gfx.op else {
            return;
        };
        if p.token != token {
            return;
        }
        let Some(Op::Ask(p, payload, work)) = self.gfx.op.take() else {
            return;
        };
        if allow {
            self.gfx_submit(p, payload, work);
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
        let (img, hash) = match outcome {
            Outcome::Denied => return,
            Outcome::Failed(e) => {
                self.gfx_reply(p.reply, Err(e), p.quiet);
                return;
            }
            Outcome::Frame(img, delta) => {
                let res = self.frame_done(&p, img, delta);
                self.images_changed(p.part);
                self.gfx_reply(p.reply, res, p.quiet);
                return;
            }
            Outcome::Decoded(d) => {
                self.iterm_done(p, d);
                return;
            }
            Outcome::Image(img, hash) => (img, hash),
        };
        if p.query {
            self.gfx_reply(p.reply, Ok(()), p.quiet);
            return;
        }
        // Retransmitting an id replaces the image and removes its placements.
        self.remove_places(|q| q.part == p.part && q.image == p.id);
        self.remove_virt(|v| v.part == p.part && v.image == p.id);
        self.sync_visible();
        let res = match self
            .gfx
            .store
            .insert_hashed(p.part, p.id, p.number, img, hash)
        {
            Err(e) => Err(e.code()),
            Ok(ins) => {
                for (s, id) in ins.evicted {
                    self.remove_places(|q| q.part == s && q.image == id);
                    self.remove_virt(|v| v.part == s && v.image == id);
                }
                match &p.place {
                    Some(spec) => self.place(p.part, p.id, p.reply.placement, spec),
                    None => Ok(()),
                }
            }
        };
        self.gfx_reply(p.reply, res, p.quiet);
    }

    fn frame_done(
        &mut self,
        p: &Pending,
        img: Image,
        delta: Option<[u32; 4]>,
    ) -> Result<(), ErrCode> {
        let PendKind::Frame { index, gap } = p.kind else {
            return Err(ErrCode::Einval);
        };
        if !self.gfx.store.contains(p.part, p.id) {
            return Err(ErrCode::Enoent);
        }
        match index {
            Some(i) => self.gfx.store.edit_frame(p.part, p.id, i, img, delta, gap),
            None => {
                let now = self.gfx.ms(self.now);
                let frame = apoth_image::anim::Frame {
                    image: Arc::new(img),
                    serial: frame_serial(),
                    gap: gap.unwrap_or(DEFAULT_GAP_MS),
                    delta,
                };
                self.gfx
                    .store
                    .add_frame(p.part, p.id, frame, now)
                    .map(|_| ())
            }
        }
    }

    // ---- sessions (T-16 §1.1) ------------------------------------------------------------

    /// `ESC _ M a=b,i=<n> ESC \` opens session `n`, `a=e,i=<n>` closes it cleanly. Other
    /// `M` commands are the media placement protocol (M34) and are ignored here.
    pub(crate) fn gfx_session(&mut self, buf: &[u8]) {
        let Some(body) = buf.strip_prefix(b"M") else {
            return;
        };
        let (mut verb, mut id, mut pid) = (0u8, 0u64, 0i32);
        for kv in body.split(|&b| b == b',') {
            match kv {
                [b'a', b'=', v] => verb = *v,
                [b'p', b'=', rest @ ..] if !rest.is_empty() && rest.len() <= 10 => {
                    pid = std::str::from_utf8(rest)
                        .ok()
                        .and_then(|t| t.parse().ok())
                        .filter(|&p: &i32| p > 0)
                        .unwrap_or(0);
                }
                [b'i', b'=', rest @ ..] if !rest.is_empty() && rest.len() <= 10 => {
                    id = std::str::from_utf8(rest)
                        .ok()
                        .and_then(|t| t.parse().ok())
                        .unwrap_or(0);
                }
                _ => {}
            }
        }
        if id == 0 {
            return;
        }
        match verb {
            b'b' => {
                self.gfx.sessions.retain(|s| s.id != id);
                if self.gfx.sessions.len() >= 16 {
                    self.gfx.sessions.remove(0);
                }
                self.gfx.sessions.push(Session {
                    id,
                    pgid: 0,
                    pid,
                    watched: false,
                });
            }
            b'e' => self.session_end(id, false),
            _ => {}
        }
    }

    /// Close a session: `gone` (its process left without `a=e`) removes every placement it
    /// made; a clean end removes only its animated ones and leaves still images.
    pub(crate) fn session_end(&mut self, id: u64, gone: bool) {
        if !self.gfx.sessions.iter().any(|s| s.id == id) {
            return;
        }
        self.gfx.sessions.retain(|s| s.id != id);
        let store = &self.gfx.store;
        let animated =
            |part: Part, image: u32| store.frame_count(part, image).is_some_and(|n| n > 1);
        let hit: HashSet<u64> = self
            .gfx
            .places
            .iter()
            .filter(|p| p.session == id && (gone || animated(p.part, p.image)))
            .map(|p| p.seq)
            .collect();
        let vhit: HashSet<u64> = self
            .gfx
            .virt
            .iter()
            .filter(|v| v.session == id && (gone || animated(v.part, v.image)))
            .map(|v| v.seq)
            .collect();
        self.remove_virt(|v| vhit.contains(&v.seq));
        self.remove_places(|q| hit.contains(&q.seq));
        for p in &mut self.gfx.places {
            if p.session == id {
                p.session = 0;
            }
        }
        for v in &mut self.gfx.virt {
            if v.session == id {
                v.session = 0;
            }
        }
    }

    // ---- iTerm2 ------------------------------------------------------------------------

    /// `OSC 1337 ; File=…:<base64>` (T-15 §3). Refused files and errors have no reply
    /// channel: they are dropped.
    pub(crate) fn gfx_iterm(&mut self, buf: &[u8]) {
        if !self.cfg.graphics.iterm2 || self.cfg.deny & deny::GRAPHICS_DIRECT != 0 {
            return;
        }
        let Ok((file, data)) = parse_file(buf) else {
            return;
        };
        let part = part_of(self.cur);
        let id = self.gfx.store.next_auto_id(part);
        let token = self.gfx.next_token;
        self.gfx.next_token += 1;
        let p = Pending {
            token,
            reply: Ids::default(),
            quiet: 2,
            spec: TransmitSpec {
                format: apoth_image::kitty::Format::Png,
                medium: Medium::Direct,
                compressed: false,
                width: 0,
                height: 0,
                size: 0,
                offset: 0,
            },
            place: None,
            query: false,
            part,
            id,
            number: 0,
            kind: PendKind::ITerm(file.clone()),
        };
        self.gfx_submit(p, data.to_vec(), Work::ITerm(file));
    }

    fn iterm_done(&mut self, p: Pending, d: apoth_image::formats::Decoded) {
        let PendKind::ITerm(file) = &p.kind else {
            return;
        };
        let mut frames = d.frames.into_iter();
        let Some((root, gap0)) = frames.next() else {
            return;
        };
        let (cols, rows) = cell_box(
            file,
            root.width,
            root.height,
            self.cfg.cell_px,
            (self.cols as u32, self.rows as u32),
        );
        let hash = root.content_hash();
        self.sync_visible();
        let Ok(ins) = self.gfx.store.insert_hashed(p.part, p.id, 0, root, hash) else {
            return;
        };
        for (s, id) in ins.evicted {
            self.remove_places(|q| q.part == s && q.image == id);
            self.remove_virt(|v| v.part == s && v.image == id);
        }
        let now = self.gfx.ms(self.now);
        let mut animated = false;
        for (img, gap) in frames {
            let f = apoth_image::anim::Frame {
                image: Arc::new(img),
                serial: frame_serial(),
                gap,
                delta: None,
            };
            if self.gfx.store.add_frame(p.part, p.id, f, now).is_err() {
                break;
            }
            animated = true;
        }
        if animated {
            let _ = self.gfx.store.animate(p.part, p.id, 3, 1, 1, gap0, 0, now);
        }
        let spec = PlaceSpec {
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            cols,
            rows,
            x_off: 0,
            y_off: 0,
            z: 0,
            no_cursor_move: false,
            unicode: false,
            relative: None,
        };
        let _ = self.place(p.part, p.id, 0, &spec);
    }

    // ---- placements --------------------------------------------------------------------

    /// Place image `id` at the cursor (`a=p`, or the display half of `a=T`), as a virtual
    /// placement (`U=1`), or relative to a parent (`P=`).
    fn place(&mut self, part: Part, id: u32, pid: u32, spec: &PlaceSpec) -> Result<(), ErrCode> {
        if spec.unicode && spec.relative.is_some() {
            return Err(ErrCode::Einval);
        }
        let img = self.gfx.store.get(part, id).ok_or(ErrCode::Enoent)?;
        let g = geom(img.width, img.height, spec, self.cfg.cell_px)?;
        if let Some(r) = spec.relative {
            self.check_parent(part, id, pid, &r)?;
        }
        if pid != 0 {
            self.remove_places(|q| q.part == part && q.image == id && q.pid == pid);
            self.remove_virt(|v| v.part == part && v.image == id && v.pid == pid);
        }
        let pid = if pid != 0 {
            pid
        } else {
            let p = self.gfx.next_pid;
            self.gfx.next_pid = if p == u32::MAX { AUTO_ID_BASE } else { p + 1 };
            p
        };
        if spec.unicode {
            self.gfx.store.place(part, id, pid, spec.clone())?;
            self.gfx.seq += 1;
            let seq = self.gfx.seq;
            let session = self.gfx.session();
            self.gfx.virt.push(Virt {
                part,
                image: id,
                pid,
                spec: spec.clone(),
                seq,
                session,
            });
            self.images_changed(part);
            return Ok(());
        }
        if let Some(r) = spec.relative {
            self.gfx.store.place(part, id, pid, spec.clone())?;
            self.gfx.seq += 1;
            let seq = self.gfx.seq;
            let session = self.gfx.session();
            self.gfx.places.push(Place {
                part,
                image: id,
                pid,
                pin: None,
                geom: g,
                z: spec.z,
                sixel: false,
                seq,
                rel: Some(r),
                session,
            });
            self.images_changed(part);
            return Ok(()); // the cursor never moves for a relative placement
        }
        self.anchor(part, id, pid, g, spec.z, false, spec.clone())?;
        if !spec.no_cursor_move {
            for _ in 0..g.rows {
                self.index();
            }
            self.cursor_right(g.cols as usize);
        }
        Ok(())
    }

    /// `ENOPARENT` when the parent is missing, `ECYCLE` when `(id, pid)` is on its own
    /// parent chain, `ETOODEEP` past [`MAX_DEPTH`].
    fn check_parent(&self, part: Part, id: u32, pid: u32, r: &Relative) -> Result<(), ErrCode> {
        let mut cur = (r.parent_id, r.parent_placement);
        for depth in 1.. {
            if pid != 0 && cur == (id, pid) {
                return Err(ErrCode::Ecycle);
            }
            if depth > MAX_DEPTH {
                return Err(ErrCode::Etoodeep);
            }
            let parent = self.find_parent(part, cur.0, cur.1);
            match parent {
                None => {
                    return Err(if depth == 1 {
                        ErrCode::Enoparent
                    } else {
                        ErrCode::Ecycle
                    });
                }
                Some(Parent::Virt) | Some(Parent::Anchored) => return Ok(()),
                Some(Parent::Rel(next)) => cur = (next.parent_id, next.parent_placement),
            }
        }
        Ok(())
    }

    /// The placement `(image, pid)` (`pid` 0: the oldest placement of the image).
    fn find_parent(&self, part: Part, image: u32, pid: u32) -> Option<Parent> {
        let hit = |i: u32, p: u32| i == image && (pid == 0 || p == pid);
        if let Some(q) = self
            .gfx
            .places
            .iter()
            .filter(|q| q.part == part && hit(q.image, q.pid))
            .min_by_key(|q| q.seq)
        {
            return Some(match q.rel {
                Some(r) => Parent::Rel(r),
                None => Parent::Anchored,
            });
        }
        self.gfx
            .virt
            .iter()
            .any(|v| v.part == part && hit(v.image, v.pid))
            .then_some(Parent::Virt)
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
        let session = self.gfx.session();
        self.gfx.places.push(Place {
            part,
            image: id,
            pid,
            pin: Some(pin),
            geom: g,
            z,
            sixel,
            seq,
            rel: None,
            session,
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
        self.remove_virt(|v| !live.contains(&(v.part, v.image, v.pid)));
        self.remove_places(|q| !live.contains(&(q.part, q.image, q.pid)));
    }

    /// Remove virtual placements matching `f` (and, through [`Self::remove_places`], the
    /// children left without a parent).
    fn remove_virt(&mut self, f: impl Fn(&Virt) -> bool) {
        if !self.gfx.virt.iter().any(&f) {
            return;
        }
        let old = std::mem::take(&mut self.gfx.virt);
        for v in old {
            if f(&v) {
                self.gfx.store.drop_placement(v.part, v.image, v.pid);
                self.panes[grid_of(v.part)].grid.mark_full();
            } else {
                self.gfx.virt.push(v);
            }
        }
        self.remove_places(|_| false);
    }

    /// Remove placements matching `f`: untrack their pins, drop them from the store, and
    /// free sixel images left without a slice. Relative placements whose parent went are
    /// removed with it, recursively.
    fn remove_places(&mut self, f: impl Fn(&Place) -> bool) {
        let orphans = |g: &Graphics| {
            g.places.iter().any(|q| {
                q.rel.is_some_and(|r| {
                    !g.places.iter().any(|o| {
                        o.part == q.part
                            && o.image == r.parent_id
                            && (r.parent_placement == 0 || o.pid == r.parent_placement)
                    }) && !g.virt.iter().any(|o| {
                        o.part == q.part
                            && o.image == r.parent_id
                            && (r.parent_placement == 0 || o.pid == r.parent_placement)
                    })
                })
            })
        };
        if !self.gfx.places.iter().any(&f) && !orphans(&self.gfx) {
            return;
        }
        let mut freed: Vec<(Part, u32)> = Vec::new();
        let mut first = true;
        loop {
            let old = std::mem::take(&mut self.gfx.places);
            let keep: Vec<bool> = old
                .iter()
                .map(|p| {
                    if first && f(p) {
                        return false;
                    }
                    p.rel.is_none_or(|r| {
                        let hit = |i: u32, q: u32| {
                            i == r.parent_id && (r.parent_placement == 0 || q == r.parent_placement)
                        };
                        old.iter().any(|o| {
                            o.seq != p.seq
                                && o.part == p.part
                                && hit(o.image, o.pid)
                                && !(first && f(o))
                        }) || self
                            .gfx
                            .virt
                            .iter()
                            .any(|o| o.part == p.part && hit(o.image, o.pid))
                    })
                })
                .collect();
            let mut removed = false;
            for (p, k) in old.into_iter().zip(keep) {
                if k {
                    self.gfx.places.push(p);
                    continue;
                }
                removed = true;
                let gi = grid_of(p.part);
                if let Some(pin) = p.pin {
                    self.panes[gi].grid.untrack_pin(pin);
                }
                self.panes[gi].grid.mark_full();
                self.gfx.store.drop_placement(p.part, p.image, p.pid);
                if p.sixel {
                    freed.push((p.part, p.image));
                }
            }
            first = false;
            if !removed || !orphans(&self.gfx) {
                break;
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
        let (abs, col) = self.place_origin(p, None, 0)?;
        let row = abs - g.history_rows() as i64;
        Some((row, usize::try_from(col).ok()?, p.geom.rows, p.geom.cols))
    }

    /// Absolute (row, col) of a placement's top-left cell. A relative placement adds its
    /// `H, V` offsets to its parent's; a virtual parent's origin comes from `virt` (the
    /// placeholder rectangles drawn this frame), else it has none.
    fn place_origin(
        &self,
        p: &Place,
        virt: Option<&[VirtOrigin]>,
        depth: usize,
    ) -> Option<(i64, i64)> {
        let Some(r) = p.rel else {
            let (abs, col) = self.panes[grid_of(p.part)].grid.pin_pos(p.pin?)?;
            return Some((abs as i64, col as i64));
        };
        if depth > MAX_DEPTH {
            return None;
        }
        let hit = |i: u32, q: u32| {
            i == r.parent_id && (r.parent_placement == 0 || q == r.parent_placement)
        };
        let base = match self
            .gfx
            .places
            .iter()
            .filter(|o| o.part == p.part && o.seq != p.seq && hit(o.image, o.pid))
            .min_by_key(|o| o.seq)
        {
            Some(o) => self.place_origin(o, virt, depth + 1)?,
            None => virt?
                .iter()
                .find(|v| v.0 == p.part && hit(v.1, v.2))
                .map(|v| (v.3, v.4))?,
        };
        Some((base.0 + i64::from(r.v), base.1 + i64::from(r.h)))
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
        let mut vis: Vec<(Part, u32, u32, bool)> = self
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
        // Virtual placements are deleted only by id, number or range (never `d=a`).
        vis.extend(
            self.gfx
                .virt
                .iter()
                .map(|v| (v.part, v.image, v.pid, false)),
        );
        for (part, image, pid, on) in vis {
            self.gfx.store.set_visible(part, image, pid, on);
        }
    }

    fn kitty_delete(&mut self, d: apoth_image::kitty::DeleteSpec) {
        let part = part_of(self.cur);
        if let DeleteTarget::Frames { id, number } = d.target {
            // `d=f`/`d=F`: the frames after the root of the named image (every image when
            // none is named); placements stay.
            let target = match (id, number) {
                (0, 0) => None,
                (0, n) => match self.gfx.store.lookup_number(part, n) {
                    Some(i) => Some(i),
                    None => return,
                },
                (i, _) => Some(i),
            };
            self.gfx.store.clear_frames(part, target);
            self.images_changed(part);
            return;
        }
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
            .filter(|p| p.part == Part::Main && p.pin.is_some())
            .filter(|p| self.place_box(p).is_none_or(|(r, ..)| r < 0))
            .map(|p| p.seq)
            .collect();
        self.remove_places(|q| hit_seq(&hist, q));
        self.gfx
            .places
            .iter()
            .filter(|p| p.part == Part::Main && p.pin.is_some())
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
                (Some(pin), Some(p)) => p.pin = Some(pin),
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
        if self.gfx.places.iter().all(|p| p.pin.is_none()) {
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
            self.remove_places(|q| q.pin.is_some_and(|p| dead.contains(&p)));
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
    /// `rows` rows tall, at instant `now` (animation frames), sorted bottom to top (z, then
    /// image id, then age). Returns when the drawn pixels change next (an animation frame
    /// is due), which the caller hands to its frame scheduler: nothing else wakes for it.
    pub(crate) fn gfx_draws(
        &self,
        top_abs: usize,
        rows: usize,
        now: Instant,
        out: &mut Vec<ImageDraw>,
    ) -> Option<Instant> {
        out.clear();
        let part = part_of(self.cur);
        let (cw, ch) = self.cfg.cell_px;
        let (cw, ch) = (i64::from(cw.max(1)), i64::from(ch.max(1)));
        let ms = self.gfx.ms_at(now);
        let mut due: Option<u64> = None;
        let rects = self.placeholder_rects(part, top_abs, rows);
        // Virtual-parent origins for relative children: the top-left of each placement's
        // first run (absolute row, col).
        let mut vorig: Vec<VirtOrigin> = Vec::new();
        for (r, pid) in &rects {
            let (y, x) = ((top_abs + r.y) as i64, r.x as i64);
            match vorig.iter_mut().find(|o| o.1 == r.image && o.2 == *pid) {
                Some(o) => {
                    o.3 = o.3.min(y);
                    o.4 = o.4.min(x);
                }
                None => vorig.push((part, r.image, *pid, y, x)),
            }
        }
        enum Src<'a> {
            Place(&'a Place),
            Cells(&'a placeholder::Rect, &'a Virt),
        }
        let mut order: Vec<(i32, u32, u64, Src<'_>, i64, i64)> = Vec::new();
        for p in self.gfx.places.iter().filter(|p| p.part == part) {
            let Some((abs, col)) = self.place_origin(p, Some(&vorig), 0) else {
                continue;
            };
            let vrow = abs - top_abs as i64;
            if vrow >= rows as i64 || vrow + i64::from(p.geom.rows) <= 0 {
                continue;
            }
            order.push((p.z, p.image, p.seq, Src::Place(p), vrow, col));
        }
        for (r, pid) in &rects {
            let Some(v) = self
                .gfx
                .virt
                .iter()
                .find(|v| v.part == part && v.image == r.image && v.pid == *pid)
            else {
                continue;
            };
            order.push((
                v.spec.z,
                v.image,
                v.seq,
                Src::Cells(r, v),
                r.y as i64,
                r.x as i64,
            ));
        }
        order.sort_by_key(|o| (o.0, o.1, o.2));
        let c = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        for (z, image, _, src, vrow, col) in order {
            let Some(view) = self.gfx.store.view(part, image, ms) else {
                continue;
            };
            if let Some(d) = view.due {
                due = Some(due.map_or(d, |x| x.min(d)));
            }
            match src {
                Src::Place(p) => {
                    let g = &p.geom;
                    let (c0w, c0h) = (i64::from(g.cell.0.max(1)), i64::from(g.cell.1.max(1)));
                    let sc = |v: i32, num: i64, den: i64| {
                        let v = i64::from(v) * num;
                        (v + den / 2).div_euclid(den)
                    };
                    let x = col * cw + sc(g.dst[0], cw, c0w);
                    let y = vrow * ch + sc(g.dst[1], ch, c0h);
                    let w = sc(g.dst[2], cw, c0w).max(1);
                    let h = sc(g.dst[3], ch, c0h).max(1);
                    out.push(ImageDraw {
                        key: view.key,
                        image: view.image,
                        dst: [c(x), c(y), c(w), c(h)],
                        src: g.src,
                        z,
                        replaces: view.replaces,
                        dirty: view.dirty,
                    });
                }
                Src::Cells(r, v) => {
                    if let Some((dst, srcr)) = cells_quad(&view.image, &v.spec, r, (cw, ch)) {
                        out.push(ImageDraw {
                            key: view.key,
                            image: view.image,
                            dst: [
                                c(col * cw + dst[0]),
                                c(vrow * ch + dst[1]),
                                c(dst[2]),
                                c(dst[3]),
                            ],
                            src: srcr,
                            z,
                            replaces: view.replaces,
                            dirty: view.dirty,
                        });
                    }
                }
            }
        }
        let e = self.gfx.epoch?;
        due.map(|d| e + std::time::Duration::from_millis(d))
    }

    /// Placeholder runs on the visible rows, merged, with the placement id they resolve to.
    /// Only scanned while a virtual placement exists.
    fn placeholder_rects(
        &self,
        part: Part,
        top_abs: usize,
        rows: usize,
    ) -> Vec<(placeholder::Rect, u32)> {
        if !self.gfx.virt.iter().any(|v| v.part == part) {
            return Vec::new();
        }
        let g = &self.panes[grid_of(part)].grid;
        let mut text = Vec::new();
        let mut m = Merger::new();
        for y in 0..rows {
            m.row(y);
            let mut res = Resolver::default();
            for x in 0..self.cols {
                let Some(st) = g.cell_abs(top_abs + y, x, &mut text) else {
                    break;
                };
                if text.first() != Some(&placeholder::PLACEHOLDER) {
                    res.reset();
                    m.gap();
                    continue;
                }
                let low = match st.fg {
                    Color::Default => None,
                    Color::Palette(i) => Some(u32::from(i)),
                    Color::Rgb(r, gg, b) => {
                        Some(u32::from(r) << 16 | u32::from(gg) << 8 | u32::from(b))
                    }
                };
                let pid = match st.ul_color {
                    Color::Default => 0,
                    Color::Palette(i) => u32::from(i),
                    Color::Rgb(r, gg, b) => u32::from(r) << 16 | u32::from(gg) << 8 | u32::from(b),
                };
                let cell = placeholder::marks(&text[1..]).and_then(|mk| res.resolve(mk, low, pid));
                match cell {
                    Some(c) => m.cell(x, c),
                    None => {
                        res.reset();
                        m.gap();
                    }
                }
            }
            m.gap();
        }
        m.finish()
            .into_iter()
            .filter_map(|r| {
                // Placement id 0 in the cells: any virtual placement of the image (oldest).
                let v = self
                    .gfx
                    .virt
                    .iter()
                    .filter(|v| {
                        v.part == part
                            && v.image == r.image
                            && (r.placement == 0 || v.pid == r.placement)
                    })
                    .min_by_key(|v| v.seq)?;
                Some((r, v.pid))
            })
            .collect()
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

/// The quad for a run of placeholder cells: the image fits the placement's `c x r` cell box
/// (aspect kept, centred; the natural size when the box is unset), and the run shows the
/// part of it under its cells. `dst` is relative to the run's top-left cell.
fn cells_quad(
    img: &Image,
    spec: &PlaceSpec,
    r: &placeholder::Rect,
    cell: (i64, i64),
) -> Option<([i64; 4], [u32; 4])> {
    let (cw, ch) = cell;
    let g = geom(img.width, img.height, spec, (cw as u32, ch as u32)).ok()?;
    let (bc, br) = (i64::from(g.cols), i64::from(g.rows));
    // The image inside the box, in box pixels.
    let (ix, iy, iw, ih) = if spec.cols != 0 && spec.rows != 0 {
        (
            i64::from(g.dst[0]),
            i64::from(g.dst[1]),
            i64::from(g.dst[2]),
            i64::from(g.dst[3]),
        )
    } else {
        // Without both sides the box is the image's own cell span: fit it and centre.
        let (bw, bh) = (bc * cw, br * ch);
        let (sw, sh) = (i64::from(g.src[2].max(1)), i64::from(g.src[3].max(1)));
        let (dw, dh) = if bw * sh <= bh * sw {
            (bw, (bw * sh / sw).max(1))
        } else {
            ((bh * sw / sh).max(1), bh)
        };
        ((bw - dw) / 2, (bh - dh) / 2, dw, dh)
    };
    // The run's cells in box pixels, clipped to the image.
    let rx0 = i64::from(r.col) * cw;
    let ry0 = i64::from(r.row) * ch;
    let rx1 = rx0 + r.w as i64 * cw;
    let ry1 = ry0 + r.h as i64 * ch;
    let x0 = rx0.max(ix);
    let y0 = ry0.max(iy);
    let x1 = rx1.min(ix + iw);
    let y1 = ry1.min(iy + ih);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (sw, sh) = (i64::from(g.src[2]), i64::from(g.src[3]));
    let map = |v: i64, o: i64, len: i64, s: i64| (v - o) * s / len.max(1);
    let sx0 = map(x0, ix, iw, sw);
    let sy0 = map(y0, iy, ih, sh);
    let sx1 = map(x1, ix, iw, sw).max(sx0 + 1);
    let sy1 = map(y1, iy, ih, sh).max(sy0 + 1);
    Some((
        [x0 - rx0, y0 - ry0, x1 - x0, y1 - y0],
        [
            g.src[0] + sx0 as u32,
            g.src[1] + sy0 as u32,
            (sx1 - sx0) as u32,
            (sy1 - sy0) as u32,
        ],
    ))
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
