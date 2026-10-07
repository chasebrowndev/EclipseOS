// SPDX-License-Identifier: AGPL-3.0-only
//! The commit slot at run time: preview, arming, the card, commit
//! (COMP-19 §5–§7, COMP-10 §3.12, A-08 §5.2). **TCB.**
//!
//! `protocols/protected` holds the Wayland side and calls in here through its
//! hooks; [`super::slot`] holds the arming rule. This module is the rest:
//!
//! ```text
//!   draft_changed → preview_task → policyd → preview{id, display}
//!   tick (every frame, and every 50 ms while a slot exists):
//!       sample the §6 conditions → Arming::observe → state event, card look
//!   Enter / click on the commit control → Arming::enter
//!       Commit  → create_task{slot, preview} → task_created → committed
//!       OpenModal → the modal commit card (§7), Tab then Space commits
//!       Drop    → the card pulses; the key is gone, never queued
//! ```
//!
//! **What was shown is what is submitted.** The card draws policyd's display
//! for the preview id it holds, and Enter commits exactly that id. A new
//! draft, a held-back draft (`has_pending_draft`), a move or any other change
//! clears the preview or restarts the arming delay before Enter can count.
//!
//! The card's size is fixed per kind (A-08 §8, "a fixed-size hole"); a
//! statement or capability summary that does not fit disarms with
//! `overflow`, and Enter opens the modal card, which shows everything.
//!
//! Choices, for the owner's review:
//! - Occluded means not drawn at all, rather than drawn dimmed: the trusted
//!   pass composites above every client, so a card drawn under an occluding
//!   window would be drawn *over* it. The client's own hole shows instead.
//! - The edge marker is a bar outside the host window's left edge, beside
//!   the card, drawn only while armed and only when the host is neither
//!   maximized nor fullscreen (§5).
//! - The arming clock is the compositor's monotonic clock, not the input
//!   event's timestamp, so every condition and the Enter are measured on one
//!   clock.
//! - An unpause slot has no policyd preview: its card names the task id it
//!   will unpause, and policyd decides at commit (`unpause_task`).

use std::collections::BTreeMap;
use std::time::Duration;

use ec_policy_eval::cbor::Reader;
use ec_policy_eval::link::ToPolicyd;
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::texture::TextureBuffer;
use smithay::backend::renderer::element::{texture::TextureRenderElement, Kind as ElementKind};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size};

use super::modal::{self, Button, Modal, Role};
use super::slot::{Arming, Conditions, Disarm, Enter, State as Arm, ARM_DEFAULT_MS};
use crate::protocols::protected::{self as protected, Kind, Reason, State};
use crate::render::glyphs::{ADVANCE, CELL_H as GLYPH_H, LINE_H};
use crate::render::text::{self, Canvas, Raster};
use crate::render::AbyssRenderElement;
use crate::state::AbyssState;

/// Request ids this module sends: distinct from enforcement, panel and
/// install ids.
const REQ_BASE: u64 = (1 << 62) | (2 << 40);

/// Modal commit card tokens.
const TOKEN_BASE: u64 = (1 << 61) | (1 << 58);

const TICK: Duration = Duration::from_millis(50);
const PULSE: Duration = Duration::from_millis(300);

/// Card rows. A statement or summary longer than this does not fit (§7).
const STATEMENT_ROWS: usize = 4;
const COULD_ROWS: usize = 3;
const COLS: usize = 56;
const PAD: usize = 16;
const BORDER: usize = modal::BORDER;
const CONTROL_H: usize = GLYPH_H + 14;
const MARKER_W: i32 = 4;
const MARKER_GAP: i32 = 3;

/// The card's fixed size, logical px. Rows: phrase, package, statement,
/// deadline, summary, a status line, the commit control.
pub const fn card_size() -> (i32, i32) {
    let w = COLS * ADVANCE + 2 * (PAD + BORDER);
    let rows = 1 + 1 + STATEMENT_ROWS + 1 + COULD_ROWS + 1;
    let h = 2 * (PAD + BORDER) + modal::PHRASE_H + rows * LINE_H + 10 + CONTROL_H;
    (w as i32, h as i32)
}

/// What policyd said the slot should show (`ec-policyd/src/dispatch.rs`
/// `slot_display`). Every string is drawn through `text::sanitize_line`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shown {
    pub package_name: String,
    pub package: String,
    pub publisher: String,
    pub statement: String,
    pub deadline_ms: u64,
    pub could: Vec<String>,
    pub narrowed: bool,
    pub continuation: String,
    /// The closed task whose session this resumes (A-08 §5.4), or empty.
    pub resumes: String,
    pub untrusted_predecessor: bool,
}

pub fn decode(b: &[u8]) -> Option<Shown> {
    let mut r = Reader::new(b);
    let n = r.map_begin().ok()?;
    let mut s = Shown::default();
    for _ in 0..n {
        match r.key().ok()? {
            "continuation" => s.continuation = r.text().ok()?.to_owned(),
            "could" => {
                let k = r.array_len().ok()?;
                if k > 256 {
                    return None;
                }
                for _ in 0..k {
                    s.could.push(r.text().ok()?.to_owned());
                }
            }
            "deadline_ms" => s.deadline_ms = r.u64().ok()?,
            "narrowed" => s.narrowed = r.bool().ok()?,
            "package" => s.package = r.text().ok()?.to_owned(),
            "package_name" => s.package_name = r.text().ok()?.to_owned(),
            "publisher" => s.publisher = r.text().ok()?.to_owned(),
            "resumes" => s.resumes = r.text().ok()?.to_owned(),
            "statement" => s.statement = r.text().ok()?.to_owned(),
            "untrusted_predecessor" => s.untrusted_predecessor = r.bool().ok()?,
            _ => return None,
        }
    }
    r.map_end().ok()?;
    r.finish().ok()?;
    Some(s)
}

fn deadline_text(ms: u64) -> String {
    let m = ms / 60_000;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The idle card's explanation, pre-wrapped to [`COLS`] (a test holds it).
const IDLE_COMMIT: &[&str] = &[
    "Describe the task in the console. This card is drawn",
    "by the system, not the console: it shows exactly what",
    "will run and what it could do, and only Enter on this",
    "card starts it.",
];
const IDLE_UNPAUSE: &[&str] = &[
    "This card is drawn by the system, not the console.",
    "Only Enter on this card lets the task act again.",
];

/// A policyd refusal reason as the card says it. Known codes get words; an
/// unknown one is shown as sent, sanitised.
fn refusal(r: &str) -> String {
    match r {
        "policy_unavailable" => "Can't reach the policy service (ec-policyd)".into(),
        "agentd_unavailable" => "Can't reach the agent service (ec-agentd)".into(),
        "preview_stale" => "The draft changed: check it and press Enter again".into(),
        _ => format!("Refused: {}", text::sanitize_line(r)),
    }
}

fn wrapped(s: &str) -> Vec<String> {
    text::wrap(&text::sanitize_line(s), COLS)
}

/// Whether the shown content fits the card in place (§6 "the draft fits").
pub fn fits(s: &Shown) -> bool {
    wrapped(&s.statement).len() <= STATEMENT_ROWS && s.could.len() <= COULD_ROWS
}

/// How the card looks: what changes the raster.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Look {
    Previewing,
    Armed,
    /// Dimmed, no commit control.
    Waiting(&'static str),
    Refused(String),
    Pulse,
}

#[derive(Debug)]
struct Card {
    arming: Arming,
    kind: Kind,
    /// The preview request in flight and the slot revision it is for.
    asking: Option<(u64, u64)>,
    /// The preview on hand: id, what it shows, the revision it is for.
    preview: Option<(u64, Shown, u64)>,
    refused: Option<String>,
    /// The commit in flight.
    committing: Option<u64>,
    /// BLAKE3 of the draft as previewed: what `slot` records name it by.
    draft_hash: [u8; 32],
    sent: Option<(State, Reason)>,
    pulse_until: Option<u64>,
    /// Where to draw it this frame; `None` when it must not be drawn.
    draw: Option<Draw>,
    look: Look,
    art: BTreeMap<usize, (Look, TextureBuffer<GlesTexture>)>,
    /// When the card last started being drawn, for the fade-in.
    shown_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Draw {
    rect: Rectangle<i32, Logical>,
    marker: Option<Rectangle<i32, Logical>>,
    /// Opacity, 0..=255: the card fades in over [`FADE_MS`] once its host
    /// window has settled. Well inside the arming delay, so a card is never
    /// armed while it is still faint.
    alpha: u8,
}

/// The card's fade-in. Must stay below `slot::ARM_DEFAULT_MS` (a test holds it).
const FADE_MS: u64 = 150;

/// Every slot's trusted side, owned by `TrustedUi`.
#[derive(Debug, Default)]
pub struct Cards {
    by_slot: BTreeMap<u64, Card>,
    next: u64,
    ticking: bool,
    /// The modal card on screen: (token, slot, preview).
    modal: Option<(u64, u64, u64)>,
    marker: BTreeMap<(i32, i32), SolidColorBuffer>,
}

fn now_ms(state: &AbyssState) -> u64 {
    state.start_time.elapsed().as_millis() as u64
}

fn arm_ms(_state: &AbyssState) -> u64 {
    // Policy-owned (COMP-19 §6); the table carries no value yet, so the
    // default, which `Arming::new` clamps to the bounds anyway.
    ARM_DEFAULT_MS
}

fn card(state: &mut AbyssState, slot: u64) -> &mut Card {
    let kind = protected::kind_of(state, slot).unwrap_or(Kind::TaskCommit);
    let ms = arm_ms(state);
    state
        .trusted_ui
        .cards
        .by_slot
        .entry(slot)
        .or_insert_with(|| Card {
            arming: Arming::new(ms),
            kind,
            asking: None,
            preview: None,
            refused: None,
            committing: None,
            draft_hash: [0; 32],
            sent: None,
            pulse_until: None,
            draw: None,
            look: Look::Previewing,
            art: BTreeMap::new(),
            shown_at: None,
        })
}

/// The draft's BLAKE3 over its canonical CBOR: names it in the audit
/// record without carrying what the human typed.
fn draft_hash(d: &protected::Draft) -> [u8; 32] {
    use ec_policy_eval::cbor::{enc, MapBuilder};
    let mut m = MapBuilder::new();
    m.insert("package", enc(|w| w.text(&d.package)));
    m.insert("resumes", enc(|w| w.text(&d.resumes)));
    m.insert("deadline", enc(|w| w.u64(u64::from(d.deadline_s))));
    m.insert("narrowing", enc(|w| w.bytes(&d.narrowing)));
    m.insert("statement", enc(|w| w.text(&d.statement)));
    m.insert("continuation", enc(|w| w.text(&d.continuation)));
    *blake3::hash(&m.finish()).as_bytes()
}

fn req(state: &mut AbyssState) -> u64 {
    state.trusted_ui.cards.next += 1;
    REQ_BASE | (state.trusted_ui.cards.next & 0xff_ffff_ffff)
}

// ---- hooks from protocols/protected ---------------------------------------

pub fn size(kind: Kind) -> (i32, i32) {
    let _ = kind;
    card_size()
}

/// A new draft (or unpause target): drop the preview, ask for a new one.
pub fn draft_changed(state: &mut AbyssState, slot: u64) {
    let revision = protected::revision(state, slot).unwrap_or(0);
    let c = card(state, slot);
    c.preview = None;
    c.refused = None;
    c.asking = None;
    let kind = c.kind;
    match kind {
        Kind::TaskUnpause => {
            let task = protected::unpause_task(state, slot).unwrap_or("").to_owned();
            if !task.is_empty() {
                let shown = Shown {
                    statement: format!("Let task {task} act again."),
                    ..Shown::default()
                };
                card(state, slot).preview = Some((1, shown, revision));
            }
        }
        _ => {
            let Some(d) = protected::draft(state, slot).cloned() else {
                return;
            };
            if !d.package.is_empty() && !d.statement.is_empty() {
                let r = req(state);
                let hash = draft_hash(&d);
                let c = card(state, slot);
                c.asking = Some((r, revision));
                c.draft_hash = hash;
                crate::audit::slot(state, "preview", hash, None, None);
                crate::policy::link::send(
                    state,
                    &ToPolicyd::PreviewTask {
                        req: r,
                        slot,
                        package: d.package,
                        statement: d.statement,
                        deadline_ms: u64::from(d.deadline_s) * 1_000,
                        narrowing: d.narrowing,
                        continuation: d.continuation,
                        resumes: d.resumes,
                    },
                );
            }
        }
    }
    tick(state);
}

pub fn moved(state: &mut AbyssState, _slot: u64) {
    tick(state);
}

pub fn gone(state: &mut AbyssState, slot: u64) {
    state.trusted_ui.cards.by_slot.remove(&slot);
    crate::backend::damage_all(state);
}

pub fn enter(state: &mut AbyssState, slot: u64) {
    tick(state);
    let now = now_ms(state);
    let Some(c) = state.trusted_ui.cards.by_slot.get(&slot) else {
        return;
    };
    match c.arming.enter(now) {
        Enter::Commit { preview } => commit(state, slot, preview),
        Enter::OpenModal => open_modal_card(state, slot),
        Enter::Drop => pulse(state, slot),
    }
}

pub fn click(state: &mut AbyssState, slot: u64, x: i32, y: i32) {
    // Only the commit control commits; anywhere else on the card is inert.
    let (w, h) = card_size();
    let control_top = h - (PAD + BORDER + CONTROL_H) as i32;
    if x < 0 || x >= w || y < control_top || y >= h {
        return;
    }
    enter(state, slot);
}

fn pulse(state: &mut AbyssState, slot: u64) {
    let until = now_ms(state) + PULSE.as_millis() as u64;
    if let Some(c) = state.trusted_ui.cards.by_slot.get_mut(&slot) {
        c.pulse_until = Some(until);
    }
    tick(state);
}

fn commit(state: &mut AbyssState, slot: u64, preview: u64) {
    // The client believes it sent a newer draft than the one on screen:
    // committing the older would submit what the human did not last see.
    if protected::has_pending_draft(state, slot) {
        pulse(state, slot);
        return;
    }
    let kind = state.trusted_ui.cards.by_slot.get(&slot).map(|c| c.kind);
    let r = req(state);
    let msg = match kind {
        Some(Kind::TaskUnpause) => {
            let task = protected::unpause_task(state, slot).unwrap_or("").to_owned();
            ToPolicyd::UnpauseTask { req: r, slot, task }
        }
        _ => ToPolicyd::CreateTask {
            req: r,
            slot,
            preview,
        },
    };
    if let Some(c) = state.trusted_ui.cards.by_slot.get_mut(&slot) {
        c.committing = Some(r);
        // Spent: the next Enter needs a fresh preview and a full delay.
        c.preview = None;
    }
    tracing::info!(slot, "commit slot committed");
    crate::policy::link::send(state, &msg);
    tick(state);
}

// ---- policyd's answers ----------------------------------------------------

fn by_req(state: &AbyssState, req: u64) -> Option<(u64, bool)> {
    state.trusted_ui.cards.by_slot.iter().find_map(|(&s, c)| {
        if c.asking.is_some_and(|(r, _)| r == req) {
            Some((s, false))
        } else if c.committing == Some(req) {
            Some((s, true))
        } else {
            None
        }
    })
}

pub fn owns_req(state: &AbyssState, req: u64) -> bool {
    by_req(state, req).is_some()
}

pub fn preview(state: &mut AbyssState, req: u64, preview: u64, display: &[u8]) {
    let Some((slot, false)) = by_req(state, req) else {
        return;
    };
    let shown = decode(display);
    let c = card(state, slot);
    let Some((_, revision)) = c.asking.take() else {
        return;
    };
    match shown {
        Some(s) => c.preview = Some((preview, s, revision)),
        None => c.refused = Some("display_malformed".into()),
    }
    tick(state);
}

pub fn refused(state: &mut AbyssState, req: u64, reason: &str) {
    let Some((slot, committing)) = by_req(state, req) else {
        return;
    };
    let c = card(state, slot);
    if committing {
        c.committing = None;
        if reason == "preview_stale" {
            // Re-preview against what is now true and arm again (A-08 §5.2).
            draft_changed(state, slot);
            return;
        }
    } else {
        c.asking = None;
    }
    c.refused = Some(reason.to_owned());
    let hash = c.draft_hash;
    crate::audit::slot(state, "refuse", hash, None, None);
    tick(state);
}

pub fn created(state: &mut AbyssState, req: u64, task: &str) {
    let Some((slot, true)) = by_req(state, req) else {
        return;
    };
    let c = card(state, slot);
    c.committing = None;
    let hash = c.draft_hash;
    crate::audit::slot(state, "commit", hash, None, Some(task));
    protected::send_committed(state, slot, task);
    tick(state);
}

/// `done` for an unpause commit.
pub fn done(state: &mut AbyssState, req: u64) {
    let Some((slot, true)) = by_req(state, req) else {
        return;
    };
    card(state, slot).committing = None;
    let task = protected::unpause_task(state, slot).unwrap_or("").to_owned();
    protected::send_committed(state, slot, &task);
    tick(state);
}

// ---- sampling -------------------------------------------------------------

/// The §6/§7 conditions for `slot`, and where to draw it.
fn sample(state: &AbyssState, slot: u64) -> Option<(Conditions, Option<Draw>)> {
    let c = state.trusted_ui.cards.by_slot.get(&slot)?;
    let revision = protected::revision(state, slot).unwrap_or(0);
    let host = protected::host_of(state, slot)?;
    let rect = protected::slot_rect(state, slot);
    let window = crate::shell::window_for_surface(state, &root_of(state, &host));
    let (fullscreen, maximized) = window
        .as_ref()
        .and_then(|w| w.toplevel())
        .map(|t| {
            let s = t.current_state();
            (
                s.states.contains(xdg_toplevel::State::Fullscreen),
                s.states.contains(xdg_toplevel::State::Maximized),
            )
        })
        .unwrap_or((false, false));
    let win_geo = window.as_ref().and_then(|w| state.space.element_geometry(w));
    let visible = match (rect, win_geo, &window) {
        (Some(r), Some(g), Some(w)) => {
            let on_output = state.space.outputs().any(|o| {
                state
                    .space
                    .output_geometry(o)
                    .is_some_and(|og| og.contains_rect(r))
            });
            // Above the host in stacking order, and overlapping the card.
            let mut above = false;
            let mut seen = false;
            for e in state.space.elements() {
                if e == w {
                    seen = true;
                    continue;
                }
                if seen && state.space.element_geometry(e).is_some_and(|eg| eg.overlaps(r)) {
                    above = true;
                }
            }
            // While the host window animates (open, move, resize), the card
            // is not drawn: a trusted card sliding or scaling with a client
            // is one more shape to fake, and its rect would not match what
            // is on screen. It fades in once the window has settled.
            let settled = state.borders.anim.window_shader(w).is_none()
                && state.borders.anim.transform(w) == ec_abyss_render::anim::Transform::identity();
            on_output && g.contains_rect(r) && !above && settled
        }
        _ => false,
    };
    let preview = c
        .preview
        .as_ref()
        .filter(|(_, _, rev)| *rev == revision)
        .filter(|_| !protected::has_pending_draft(state, slot) && c.committing.is_none())
        .map(|(id, _, _)| *id);
    let fits = c.preview.as_ref().is_none_or(|(_, s, _)| fits(s));
    let scale = rect
        .and_then(|r| {
            state
                .space
                .outputs()
                .find(|o| state.space.output_geometry(o).is_some_and(|g| g.overlaps(r)))
        })
        .map(|o| (o.current_scale().fractional_scale() * 1000.0) as u64)
        .unwrap_or(0);
    let geometry = rect.map_or(0, |r| {
        let mut h = r.loc.x as u64 & 0xffff;
        h = (h << 16) | (r.loc.y as u64 & 0xffff);
        h = (h << 16) | (r.size.w as u64 & 0xff);
        (h << 8 | (r.size.h as u64 & 0xff)) ^ scale.rotate_left(48)
    });
    let conditions = Conditions {
        preview,
        preview_refused: c.refused.is_some(),
        policy_available: state.policy_key.is_some() && state.policy_table.is_some(),
        host_focused: protected::gate::focused_slot(state) == Some(slot),
        fully_visible: visible,
        geometry,
        fits,
        fullscreen,
        unlocked: !state.lock.locked,
        modal_showing: state.trusted_ui.is_open(),
    };
    let draw = rect.filter(|_| visible && !state.lock.locked).map(|r| Draw {
        rect: r,
        alpha: 0,
        marker: win_geo.filter(|_| !maximized && !fullscreen).map(|g| {
            Rectangle::new(
                (g.loc.x - MARKER_GAP - MARKER_W, r.loc.y).into(),
                (MARKER_W, r.size.h).into(),
            )
        }),
    });
    Some((conditions, draw))
}

/// Opacity `ms` into the fade-in, 0..=255, ease-out.
fn fade(ms: u64) -> u8 {
    let t = (ms as f32 / FADE_MS as f32).min(1.0);
    let e = 1.0 - (1.0 - t) * (1.0 - t);
    (e * 255.0).round() as u8
}

fn root_of(
    state: &AbyssState,
    s: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
    let _ = state;
    let mut cur = s.clone();
    while let Some(p) = smithay::wayland::compositor::get_parent(&cur) {
        cur = p;
    }
    cur
}

fn reason_of(c: &Card, arm: Arm) -> (State, Reason, Look) {
    match arm {
        Arm::Armed => (State::Armed, Reason::None, Look::Armed),
        Arm::Previewing => (State::Previewing, Reason::None, Look::Previewing),
        Arm::Disarmed(Disarm::Overflow) => (
            State::Disarmed,
            Reason::Overflow,
            Look::Waiting("Too long for this card: press Enter to review it in full"),
        ),
        Arm::Disarmed(Disarm::PolicyUnavailable) => (
            State::Disarmed,
            Reason::PolicyUnavailable,
            Look::Waiting("Can't reach the policy service (ec-policyd)"),
        ),
        Arm::Disarmed(Disarm::Locked) => (State::Disarmed, Reason::Locked, Look::Waiting("Session locked")),
        Arm::Suspended => (
            State::Suspended,
            Reason::NotFocused,
            Look::Waiting("Waiting for focus"),
        ),
        Arm::Refused => {
            let why = c.refused.clone().unwrap_or_default();
            let reason = match why.as_str() {
                "preview_stale" => Reason::PreviewStale,
                "agentd_unavailable" => Reason::AgentdUnavailable,
                "policy_unavailable" => Reason::PolicyUnavailable,
                _ => Reason::Refused,
            };
            (State::Refused, reason, Look::Refused(why))
        }
    }
}

/// The personal secret changed: every card drawn with the old one (or with
/// none) is redrawn on its next frame.
pub fn phrase_changed(state: &mut AbyssState) {
    for c in state.trusted_ui.cards.by_slot.values_mut() {
        c.art.clear();
    }
}

/// Samples every slot, moves its arming on, and tells its client and the
/// renderer what changed.
pub fn tick(state: &mut AbyssState) {
    let now = now_ms(state);
    let slots: Vec<u64> = protected::slots(state).collect();
    state.trusted_ui.cards.by_slot.retain(|s, _| slots.contains(s));
    let mut damage = false;
    for slot in slots {
        let _ = card(state, slot);
        let Some((cond, mut draw)) = sample(state, slot) else {
            continue;
        };
        let c = state.trusted_ui.cards.by_slot.get_mut(&slot).expect("just made");
        c.shown_at = draw.and(c.shown_at.or(Some(now)));
        if let (Some(d), Some(at)) = (draw.as_mut(), c.shown_at) {
            d.alpha = fade(now.saturating_sub(at));
        }
        c.arming.observe(cond, now);
        let (st, reason, mut look) = reason_of(c, c.arming.state(now));
        if c.pulse_until.is_some_and(|u| u > now) {
            look = Look::Pulse;
        } else {
            c.pulse_until = None;
        }
        if c.look != look || c.draw != draw {
            c.look = look;
            c.draw = draw;
            damage = true;
        }
        if c.sent != Some((st, reason)) {
            c.sent = Some((st, reason));
            let armed =
                (st == State::Armed).then(|| (c.draft_hash, c.preview.as_ref().map(|(id, _, _)| *id)));
            protected::send_state(state, slot, st, reason);
            if let Some((hash, preview)) = armed {
                crate::audit::slot(state, "arm", hash, preview, None);
            }
        }
    }
    if damage {
        crate::backend::damage_all(state);
    }
    schedule(state);
}

fn schedule(state: &mut AbyssState) {
    if state.trusted_ui.cards.ticking || state.trusted_ui.cards.by_slot.is_empty() {
        return;
    }
    state.trusted_ui.cards.ticking = true;
    let r = state
        .loop_handle
        .insert_source(Timer::from_duration(TICK), |_, _, state| {
            state.trusted_ui.cards.ticking = false;
            tick(state);
            TimeoutAction::Drop
        });
    if r.is_err() {
        // No tick means arming only advances on frames and input; the slot
        // can still not arm early, only late.
        state.trusted_ui.cards.ticking = false;
    }
}

// ---- the §7 modal card ----------------------------------------------------

fn modal_card(token: u64, s: &Shown) -> Option<Modal> {
    let deadline = deadline_text(s.deadline_ms);
    let mut m = Modal::new(
        token,
        "Start this task?",
        s.untrusted_predecessor
            .then_some("It continues a task that read untrusted content."),
        "The agent gets exactly what is listed here, until the deadline. Tab to Start, then Space.",
        "The package's name:",
        &s.package_name,
        vec![
            Button {
                label: "Start",
                role: Role::Grant,
            },
            Button {
                label: "Cancel",
                role: Role::Safe,
            },
        ],
    )
    .ok()?
    .with_facts(&[
        ("Agent", &s.package),
        ("Publisher", &s.publisher),
        ("Deadline", &deadline),
    ]);
    m = m.with_whole_fact("Task", &s.statement, 18).ok()?;
    if s.could.is_empty() {
        m = m.with_facts(&[("Could", "only talk with you")]);
    }
    for l in &s.could {
        m = m.with_whole_fact("Could", l, 3).ok()?;
    }
    if !s.continuation.is_empty() {
        m = m.with_facts(&[("Continues", &s.continuation)]);
    }
    if !s.resumes.is_empty() {
        m = m.with_facts(&[("Resumes", &s.resumes)]);
    }
    if s.narrowed {
        m = m.with_facts(&[("Narrowed", "yes, by the draft")]);
    }
    Some(m)
}

fn open_modal_card(state: &mut AbyssState, slot: u64) {
    let Some((preview, shown)) = state
        .trusted_ui
        .cards
        .by_slot
        .get(&slot)
        .and_then(|c| c.preview.as_ref())
        .map(|(id, s, _)| (*id, s.clone()))
    else {
        return pulse(state, slot);
    };
    state.trusted_ui.cards.next += 1;
    let token = TOKEN_BASE | (state.trusted_ui.cards.next & 0xffff_ffff);
    if modal_card(token, &shown).is_some_and(|m| super::open(state, m)) {
        state.trusted_ui.cards.modal = Some((token, slot, preview));
    } else {
        pulse(state, slot);
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    state.trusted_ui.cards.modal.is_some_and(|(t, _, _)| t == token)
}

pub fn answer(state: &mut AbyssState, choice: super::Choice) {
    let Some((_, slot, preview)) = state.trusted_ui.cards.modal.take() else {
        return;
    };
    let current = state
        .trusted_ui
        .cards
        .by_slot
        .get(&slot)
        .and_then(|c| c.preview.as_ref())
        .map(|(id, _, _)| *id);
    // Committed from the card only if it still shows the preview the card
    // was opened on.
    if choice.role == Role::Grant && !choice.timed_out && current == Some(preview) {
        commit(state, slot, preview);
    }
}

// ---- drawing --------------------------------------------------------------

fn rasterize(look: &Look, shown: Option<&Shown>, kind: Kind, phrase: Option<&str>, scale: usize) -> Raster {
    let (w, h) = card_size();
    let (w, h) = (w as usize, h as usize);
    let dim = !matches!(look, Look::Armed | Look::Pulse);
    let rim = match look {
        Look::Pulse | Look::Refused(_) => (2, modal::WARN),
        _ => (1, modal::EDGE),
    };
    let body = if dim { modal::LABEL } else { modal::BODY };
    let mut c = Canvas::new(w, h);
    c.round(0, 0, w, h, modal::FRAME_R, modal::PANEL, Some(rim));
    let x0 = BORDER + PAD;
    let inner = w - 2 * x0;
    let mut y = BORDER + PAD;
    modal::phrase_frame(
        &mut c,
        x0 + modal::WELL_PAD,
        y,
        inner - 2 * modal::WELL_PAD,
        phrase,
    );
    y += modal::PHRASE_H + 8;
    match (kind, shown) {
        (_, Some(s)) => {
            let who = if s.package_name.is_empty() {
                s.package.clone()
            } else {
                format!(
                    "{} ({}, {})",
                    text::sanitize_line(&s.package_name),
                    s.package,
                    s.publisher
                )
            };
            c.text(x0, y, &who, modal::HEADING, true);
            y += LINE_H;
            for (i, l) in wrapped(&s.statement).iter().take(STATEMENT_ROWS).enumerate() {
                c.text(x0, y + i * LINE_H, l, body, false);
            }
            y += STATEMENT_ROWS * LINE_H;
            if kind == Kind::TaskCommit {
                c.text(
                    x0,
                    y,
                    &format!("Until: {}", deadline_text(s.deadline_ms)),
                    modal::LABEL,
                    false,
                );
            }
            y += LINE_H;
            if s.could.is_empty() && kind == Kind::TaskCommit {
                c.text(x0, y, "Could: only talk with you", body, false);
            }
            for (i, l) in s.could.iter().take(COULD_ROWS).enumerate() {
                let line: String = format!("Could: {}", text::sanitize_line(l))
                    .chars()
                    .take(COLS)
                    .collect();
                c.text(x0, y + i * LINE_H, &line, body, false);
            }
            y += COULD_ROWS * LINE_H;
            if s.untrusted_predecessor {
                let line = if s.resumes.is_empty() {
                    "Continues a task that read untrusted content."
                } else {
                    "Resumes a session that may hold untrusted content."
                };
                c.text(x0, y, line, modal::WARN, false);
            }
        }
        (_, None) => {
            // Nothing to show yet: say what this card is, in fixed text, so
            // an empty card is not a mystery box.
            let (head, lines) = match kind {
                Kind::TaskUnpause => ("Let a paused task act again", IDLE_UNPAUSE),
                _ => ("Start an agent task", IDLE_COMMIT),
            };
            c.text(x0, y, head, modal::HEADING, true);
            for (i, l) in lines.iter().enumerate() {
                c.text(x0, y + (i + 1) * LINE_H, l, modal::LABEL, false);
            }
            y += (1 + STATEMENT_ROWS + 1 + COULD_ROWS) * LINE_H;
        }
    }
    y += LINE_H;
    let status: String = match look {
        Look::Previewing => "Checking with policy...".into(),
        Look::Armed => String::new(),
        Look::Pulse => "Not ready: nothing was sent.".into(),
        Look::Waiting(w) => (*w).into(),
        Look::Refused(r) => refusal(r),
    };
    let _ = y;
    let cy = h - BORDER - PAD - CONTROL_H;
    if matches!(look, Look::Armed) {
        c.round(x0, cy, inner, CONTROL_H, modal::PILL, modal::GOLD, None);
        let label = if kind == Kind::TaskUnpause {
            "Enter: let it act again"
        } else {
            "Enter: start this task"
        };
        c.text(
            x0 + 16,
            cy + (CONTROL_H - GLYPH_H) / 2 + 1,
            label,
            modal::INK,
            true,
        );
    } else {
        c.text(
            x0,
            cy + (CONTROL_H - GLYPH_H) / 2,
            &status,
            if matches!(look, Look::Refused(_) | Look::Pulse) {
                modal::WARN
            } else {
                modal::LABEL
            },
            false,
        );
    }
    c.into_raster(scale)
}

/// Whether `p` (global logical) is on a card that is drawn. The DRM path
/// then draws the compositor's own arrow above the cards and hides the
/// client's cursor, as it does under a modal: a client cursor drawn over a
/// card could cover it with pixels the client chose (COMP-10 §5), and one
/// drawn under it is invisible.
pub fn under_pointer(cards: &Cards, p: Point<f64, Logical>) -> bool {
    cards.by_slot.values().any(|c| {
        c.draw.is_some_and(|d| {
            let r = d.rect.to_f64();
            p.x >= r.loc.x && p.y >= r.loc.y && p.x < r.loc.x + r.size.w && p.y < r.loc.y + r.size.h
        })
    })
}

/// The rects of the cards drawn now, for tests.
#[cfg(test)]
pub fn drawn(cards: &Cards) -> Vec<Rectangle<i32, Logical>> {
    cards
        .by_slot
        .values()
        .filter_map(|c| c.draw.map(|d| d.rect))
        .collect()
}

/// Sample cards for `dump_trusted_surfaces` (mod.rs), at 2x.
#[cfg(test)]
pub(super) fn dump_cards() -> Vec<(&'static str, Raster)> {
    let shown = Shown {
        package_name: "Inbox triage".into(),
        package: "local.inbox".into(),
        publisher: "local".into(),
        statement:
            "Sort this week's unread mail into folders and draft replies to anything from the accountant."
                .into(),
        deadline_ms: 0,
        could: vec![
            "read mail in Thunderbird".into(),
            "draft replies (not send)".into(),
        ],
        ..Default::default()
    };
    vec![
        (
            "card-armed",
            rasterize(
                &Look::Armed,
                Some(&shown),
                Kind::TaskCommit,
                Some("blue heron 42"),
                2,
            ),
        ),
        (
            "card-unavailable",
            rasterize(
                &Look::Waiting("Can't reach the policy service (ec-policyd)"),
                None,
                Kind::TaskCommit,
                None,
                2,
            ),
        ),
        (
            "card-refused",
            rasterize(
                &Look::Refused("deadline over the policy maximum".into()),
                Some(&shown),
                Kind::TaskCommit,
                Some("blue heron 42"),
                2,
            ),
        ),
    ]
}

/// The cards for one output, front-to-back, to sit below any modal and above
/// every client.
pub fn elements(
    renderer: &mut GlesRenderer,
    ui: &mut super::TrustedUi,
    output: &Output,
    output_loc: Point<i32, Logical>,
    output_size: Size<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    let dev = (fractional.round() as usize).max(1);
    let phrase = ui.phrase.as_ref().map(|p| p.as_str().to_owned());
    let out_rect = Rectangle::new(output_loc, output_size);
    let mut out = Vec::new();
    let cards = &mut ui.cards;
    for c in cards.by_slot.values_mut() {
        let Some(d) = c.draw else { continue };
        if !out_rect.contains_rect(d.rect) {
            continue;
        }
        let stale = c.art.get(&dev).is_none_or(|(l, _)| *l != c.look);
        if stale {
            let shown = c.preview.as_ref().map(|(_, s, _)| s);
            let raster = rasterize(&c.look, shown, c.kind, phrase.as_deref(), dev);
            match super::upload(renderer, &raster, dev) {
                Some(b) => {
                    c.art.insert(dev, (c.look.clone(), b));
                }
                None => {
                    c.art.remove(&dev);
                    continue;
                }
            }
        }
        if let Some((_, buffer)) = c.art.get(&dev) {
            let local = d.rect.loc - output_loc;
            out.push(AbyssRenderElement::Texture(
                TextureRenderElement::from_texture_buffer(
                    local.to_f64().to_physical(scale),
                    buffer,
                    Some(f32::from(d.alpha) / 255.0),
                    None,
                    None,
                    ElementKind::Unspecified,
                ),
            ));
        }
        if let (Some(m), Look::Armed) = (d.marker, &c.look) {
            if out_rect.contains_rect(m) {
                let buf = cards
                    .marker
                    .entry((m.size.w, m.size.h))
                    .or_insert_with(|| SolidColorBuffer::new(m.size, modal::GOLD));
                out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                    buf,
                    (m.loc - output_loc).to_physical_precise_round(scale),
                    scale,
                    1.0,
                    ElementKind::Unspecified,
                )));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::cbor::{enc, MapBuilder};

    fn shown_bytes(statement: &str, could: &[&str]) -> Vec<u8> {
        let mut d = MapBuilder::new();
        d.insert("package_name", enc(|w| w.text("Reference agent")));
        d.insert("package", enc(|w| w.text("ec-ref-agent")));
        d.insert("publisher", enc(|w| w.text("eclipse")));
        d.insert("statement", enc(|w| w.text(statement)));
        d.insert("deadline_ms", enc(|w| w.u64(7_200_000)));
        d.insert(
            "could",
            enc(|w| {
                w.array(could.len());
                for c in could {
                    w.text(c);
                }
            }),
        );
        d.insert("narrowed", enc(|w| w.bool(false)));
        d.insert("continuation", enc(|w| w.text("")));
        d.insert("untrusted_predecessor", enc(|w| w.bool(false)));
        d.finish()
    }

    #[test]
    fn policyd_s_display_decodes_and_fits_or_overflows() {
        let s = decode(&shown_bytes("Say hello", &[])).unwrap();
        assert_eq!(s.deadline_ms, 7_200_000);
        assert!(fits(&s));
        let long = "word ".repeat(80);
        assert!(
            !fits(&decode(&shown_bytes(&long, &[])).unwrap()),
            "statement overflow"
        );
        let many = ["a x:1", "b x:2", "c x:3", "d x:4"];
        assert!(
            !fits(&decode(&shown_bytes("x", &many)).unwrap()),
            "summary overflow"
        );
        let mut b = shown_bytes("x", &[]);
        b.push(0);
        assert!(decode(&b).is_none());
    }

    #[test]
    fn the_card_is_one_fixed_size_and_draws_every_look() {
        let (w, h) = card_size();
        let s = decode(&shown_bytes("Say hello", &["seat.key app_id:foot"])).unwrap();
        for look in [
            Look::Previewing,
            Look::Armed,
            Look::Pulse,
            Look::Waiting("x"),
            Look::Refused("not_installed".into()),
        ] {
            let r = rasterize(&look, Some(&s), Kind::TaskCommit, Some("tangerine"), 1);
            assert_eq!((r.w, r.h), (w, h), "{look:?}");
        }
        assert_eq!(deadline_text(7_200_000), "2 h");
        assert_eq!(deadline_text(5_400_000), "1 h 30 min");
    }

    #[test]
    fn the_anti_spoofing_notice_fits_the_card() {
        assert!(modal::UNSPOOFED.len() * ADVANCE <= COLS * ADVANCE - 2 * modal::WELL_PAD);
    }

    #[test]
    fn the_idle_text_fits_the_card() {
        for l in IDLE_COMMIT.iter().chain(IDLE_UNPAUSE) {
            assert!(l.len() <= COLS, "{l}");
        }
        assert!(IDLE_COMMIT.len() < STATEMENT_ROWS + 1 + COULD_ROWS);
    }

    #[test]
    fn no_card_is_under_the_pointer_when_none_is_drawn() {
        assert!(!under_pointer(&Cards::default(), Point::from((10.0, 10.0))));
    }

    #[test]
    fn the_fade_in_ends_before_the_card_can_arm() {
        const { assert!(FADE_MS < super::super::slot::ARM_DEFAULT_MS) };
        assert_eq!(fade(0), 0);
        assert_eq!(fade(FADE_MS), 255);
        assert!(fade(FADE_MS / 2) > 127, "ease-out");
    }

    #[test]
    fn the_modal_card_shows_the_whole_statement_and_defaults_to_cancel() {
        let long = "word ".repeat(80);
        let s = decode(&shown_bytes(&long, &["seat.key app_id:foot"])).unwrap();
        let m = modal_card(TOKEN_BASE | 1, &s).unwrap();
        let shown: String = m
            .facts()
            .iter()
            .filter(|(l, _)| *l == "Task" || l.is_empty())
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(shown.replace(' ', ""), long.trim_end().replace(' ', ""));
        assert_eq!(m.buttons()[m.safe()].label, "Cancel");
    }
}
