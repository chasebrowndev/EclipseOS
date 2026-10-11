// SPDX-License-Identifier: AGPL-3.0-only
//! The compat-lock indicator (COMP-10 §3.4), and the indicator click that
//! opens the emergency panel (§3.3). **TCB.**
//!
//! While an agent holds a compat lock (COMP-04 §8) the locked toplevel gets a
//! thin outline in the agent's colour, and every output shows a pill in its
//! top-right corner, under the capture marker: a lock in the agent's colour,
//! the agent, the seconds left, and how many of the human's inputs are being
//! held, so it is plain why typing "isn't working".
//!
//! Like every trusted surface it is drawn by the compositor above every
//! client and never into a capture. It carries no personal phrase: it decides
//! nothing (§2). It costs nothing at idle: the tick runs only while a lock
//! exists.
//!
//! A left press on the capture marker or on a lock pill opens the panel. It
//! pauses nothing; only the override chord does (§3.3). A pointer button
//! pressed there is the compositor's, press and release, and reaches no
//! client. Touch and tablet contacts are not intercepted yet (KNOWNBUGS
//! IND-01).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Point, Rectangle, Scale, Size};

use super::modal;
use crate::render::glyphs::{ADVANCE, CELL_H as GLYPH_H};
use crate::render::text::{self, Canvas, Raster, Rgba};
use crate::render::AbyssRenderElement;
use crate::state::AbyssState;

/// The countdown has whole seconds; this only has to keep it and the held
/// count current, and the outline on a window being moved.
const TICK: Duration = Duration::from_millis(50);
/// The outline's width, outside the window's edge.
const RIM: i32 = 2;
const PILL_H: i32 = 28;
const PILL_GAP: i32 = 6;
/// Below the capture marker (8 px from the top, 8 tall).
const TOP: i32 = 24;
const EDGE_GAP: i32 = 8;
const PAD: i32 = 12;
const ICON_W: i32 = 12;
const ICON_GAP: i32 = 8;
/// The longest agent name drawn, in glyphs.
const NAME: usize = 24;
/// Extra reach around the capture marker: 8 px tall is a thin target.
const SLOP: i32 = 6;

/// §8.3: the agent's colour is a deterministic hash of its id into a
/// colourblind-safe palette (Okabe–Ito, without black and the yellow that
/// would read as the accent).
const AGENT: [Rgba; 6] = [
    [0.337, 0.706, 0.914, 1.0],
    [0.0, 0.620, 0.451, 1.0],
    [0.835, 0.369, 0.0, 1.0],
    [0.800, 0.475, 0.655, 1.0],
    [0.902, 0.624, 0.0, 1.0],
    [0.0, 0.447, 0.698, 1.0],
];

pub fn colour(agent: u64) -> Rgba {
    AGENT[((agent.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) % AGENT.len() as u64) as usize]
}

/// One lock as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    agent: u64,
    name: String,
    /// The locked window, global logical; `None` while it is not mapped.
    window: Option<Rectangle<i32, Logical>>,
    secs: u64,
    held: usize,
}

#[derive(Debug, Default)]
pub struct Marks {
    shown: Vec<Shown>,
    ticking: bool,
    /// Pointer buttons pressed on an indicator, one bit per evdev code from
    /// `BTN_MOUSE`, so their releases are swallowed too.
    pressed: u16,
    /// The uploaded pill per (device scale, agent), with what it shows.
    art: BTreeMap<(usize, u64), (Shown, TextureBuffer<GlesTexture>)>,
    /// The four outline sides per agent.
    rims: BTreeMap<(u64, usize), SolidColorBuffer>,
}

/// Re-read the locks; damage if what is drawn changed. Every backend calls
/// it each frame, and it ticks itself while a lock exists.
pub fn tick(state: &mut AbyssState) {
    let marks = &state.trusted_ui.marks;
    if marks.shown.is_empty() && state.agents.locks.active().next().is_none() {
        return;
    }
    let names = state.agents.list();
    let now = Instant::now();
    let shown: Vec<Shown> = state
        .agents
        .locks
        .shown()
        .map(|(agent, window, expires, held)| Shown {
            agent,
            name: names
                .iter()
                .find(|(i, _)| *i == agent)
                .map(|(_, p)| text::sanitize_line(p).chars().take(NAME).collect())
                .unwrap_or_else(|| format!("#{agent}")),
            window: state.space.element_geometry(window),
            secs: expires.saturating_duration_since(now).as_millis().div_ceil(1000) as u64,
            held,
        })
        .collect();
    if shown != state.trusted_ui.marks.shown {
        state.trusted_ui.marks.shown = shown;
        crate::backend::damage_all(state);
    }
    schedule(state);
}

fn schedule(state: &mut AbyssState) {
    let marks = &mut state.trusted_ui.marks;
    if marks.ticking || marks.shown.is_empty() {
        return;
    }
    marks.ticking = true;
    let r = state
        .loop_handle
        .insert_source(Timer::from_duration(TICK), |_, _, state| {
            state.trusted_ui.marks.ticking = false;
            tick(state);
            TimeoutAction::Drop
        });
    if r.is_err() {
        // No timer: the frame-path tick still updates it, only less often.
        state.trusted_ui.marks.ticking = false;
    }
}

fn label(s: &Shown) -> (String, String, String) {
    let held = if s.held > 0 {
        format!("  {} held", s.held)
    } else {
        String::new()
    };
    (s.name.clone(), format!("  locked a window  {}s", s.secs), held)
}

fn pill_w(s: &Shown) -> i32 {
    let (a, b, c) = label(s);
    let chars = a.chars().count() + b.len() + c.len();
    PAD + ICON_W + ICON_GAP + (chars * ADVANCE) as i32 + PAD
}

/// The `i`th pill, output-local, on an output `logical` in size.
fn pill_rect(s: &Shown, i: usize, logical: Size<i32, Logical>) -> Rectangle<i32, Logical> {
    let w = pill_w(s);
    Rectangle::new(
        (logical.w - w - EDGE_GAP, TOP + i as i32 * (PILL_H + PILL_GAP)).into(),
        (w, PILL_H).into(),
    )
}

fn rasterize(s: &Shown, scale: usize) -> Raster {
    let (w, h) = (pill_w(s) as usize, PILL_H as usize);
    let mut c = Canvas::new(w, h);
    c.round(0, 0, w, h, modal::PILL, modal::PANEL, Some((1, modal::EDGE)));
    let tint = colour(s.agent);
    let (ix, iy) = (PAD as usize, (h - 15) / 2);
    // A padlock: shackle outline, then the body over its lower half.
    c.round(ix + 2, iy, 8, 10, 4, [0.0; 4], Some((2, tint)));
    c.round(ix, iy + 6, ICON_W as usize, 9, 2, tint, None);
    let (name, rest, held) = label(s);
    let mut x = (PAD + ICON_W + ICON_GAP) as usize;
    let y = (h - GLYPH_H) / 2;
    c.text(x, y, &name, modal::HEADING, true);
    x += name.chars().count() * ADVANCE;
    c.text(x, y, &rest, modal::BODY, false);
    x += rest.len() * ADVANCE;
    c.text(x, y, &held, modal::WARN, false);
    c.into_raster(scale)
}

/// The outlines and pills for `output`. Front-most first, like every pass.
pub(super) fn elements(
    renderer: &mut GlesRenderer,
    marks: &mut Marks,
    output: &Output,
    output_loc: Point<i32, Logical>,
    logical: Size<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    if marks.shown.is_empty() {
        return Vec::new();
    }
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    let dev = (fractional.round() as usize).max(1);
    let out_rect = Rectangle::new(output_loc, logical);
    let mut out = Vec::new();
    for (i, s) in marks.shown.iter().enumerate() {
        let stale = marks.art.get(&(dev, s.agent)).is_none_or(|(was, _)| was != s);
        if stale {
            match super::upload(renderer, &rasterize(s, dev), dev) {
                Some(b) => {
                    marks.art.insert((dev, s.agent), (s.clone(), b));
                }
                None => {
                    tracing::error!("compat-lock indicator upload failed");
                    marks.art.remove(&(dev, s.agent));
                }
            }
        }
        if let Some((_, buffer)) = marks.art.get(&(dev, s.agent)) {
            let loc = pill_rect(s, i, logical).loc;
            out.push(AbyssRenderElement::Texture(
                TextureRenderElement::from_texture_buffer(
                    loc.to_f64().to_physical(scale),
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ),
            ));
        }
    }
    for s in &marks.shown {
        let Some(g) = s.window else { continue };
        let outer = Rectangle::new(
            g.loc - Point::from((RIM, RIM)),
            g.size + Size::from((2 * RIM, 2 * RIM)),
        );
        if !out_rect.overlaps(outer) {
            continue;
        }
        let sides = [
            Rectangle::new(outer.loc, (outer.size.w, RIM).into()),
            Rectangle::new(
                (outer.loc.x, outer.loc.y + outer.size.h - RIM).into(),
                (outer.size.w, RIM).into(),
            ),
            Rectangle::new((outer.loc.x, g.loc.y).into(), (RIM, g.size.h).into()),
            Rectangle::new((g.loc.x + g.size.w, g.loc.y).into(), (RIM, g.size.h).into()),
        ];
        for (k, r) in sides.into_iter().enumerate() {
            let buf = marks
                .rims
                .entry((s.agent, k))
                .or_insert_with(|| SolidColorBuffer::new(r.size, colour(s.agent)));
            buf.resize(r.size);
            out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                buf,
                (r.loc - output_loc).to_physical_precise_round(scale),
                scale,
                1.0,
                Kind::Unspecified,
            )));
        }
    }
    let live: Vec<u64> = marks.shown.iter().map(|s| s.agent).collect();
    marks.rims.retain(|(a, _), _| live.contains(a));
    marks.art.retain(|(_, a), _| live.contains(a));
    out
}

/// Whether `p` (global logical) is on the capture marker while it is lit, or
/// on a lock pill.
pub fn at(state: &AbyssState, p: Point<f64, Logical>) -> bool {
    let capture = state.capture_active();
    let marks = &state.trusted_ui.marks;
    if !capture && marks.shown.is_empty() {
        return false;
    }
    let hit = |r: Rectangle<i32, Logical>| {
        let r = r.to_f64();
        p.x >= r.loc.x && p.y >= r.loc.y && p.x < r.loc.x + r.size.w && p.y < r.loc.y + r.size.h
    };
    state.space.outputs().any(|out| {
        let Some(geo) = state.space.output_geometry(out) else {
            return false;
        };
        let on_capture = capture
            && crate::render::capture::indicator_rect(out).is_some_and(|r| {
                hit(Rectangle::new(
                    geo.loc + r.loc - Point::from((SLOP, SLOP)),
                    r.size + Size::from((2 * SLOP, 2 * SLOP)),
                ))
            });
        on_capture
            || super::logical_size(out).is_some_and(|logical| {
                marks.shown.iter().enumerate().any(|(i, s)| {
                    let r = pill_rect(s, i, logical);
                    hit(Rectangle::new(geo.loc + r.loc, r.size))
                })
            })
    })
}

const BTN_MOUSE: u32 = 0x110;

/// A human pointer button. True if an indicator took it: a press on one, or
/// the release of a press that was. The left button opens the emergency
/// panel; no client sees any of them.
pub fn button(state: &mut AbyssState, button: u32, pressed: bool) -> bool {
    let bit = button
        .checked_sub(BTN_MOUSE)
        .filter(|b| *b < 16)
        .map_or(0, |b| 1u16 << b);
    if !pressed {
        let took = state.trusted_ui.marks.pressed & bit != 0;
        state.trusted_ui.marks.pressed &= !bit;
        return took;
    }
    if !at(state, state.pointer_location) {
        return false;
    }
    state.trusted_ui.marks.pressed |= bit;
    if button == BTN_MOUSE {
        super::panel::open(state);
    }
    true
}

/// How many locks are drawn, and the held count and outline of each.
#[cfg(test)]
pub(crate) fn drawn(state: &AbyssState) -> Vec<(usize, Option<Rectangle<i32, Logical>>)> {
    state
        .trusted_ui
        .marks
        .shown
        .iter()
        .map(|s| (s.held, s.window))
        .collect()
}

/// A sample pill for `dump_trusted_surfaces` (mod.rs), at 2x.
#[cfg(test)]
pub(super) fn dump_pill() -> Raster {
    rasterize(
        &Shown {
            agent: 7,
            name: "agent:research-7".into(),
            window: None,
            secs: 8,
            held: 3,
        },
        2,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_keeps_its_colour() {
        for id in 0..64 {
            assert_eq!(colour(id), colour(id));
            assert!(AGENT.contains(&colour(id)));
        }
    }

    /// §3.3: clicking the capture marker opens the emergency panel, the
    /// press reaches no client, and a dark marker is no target at all.
    #[test]
    fn clicking_the_capture_marker_opens_the_panel() {
        use smithay::backend::input::ButtonState;
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let out = s.space.outputs().next().unwrap().clone();
        let geo = s.space.output_geometry(&out).unwrap();
        let r = crate::render::capture::indicator_rect(&out).unwrap();
        s.pointer_location = (geo.loc + r.loc + Point::from((r.size.w / 2, r.size.h / 2))).to_f64();

        assert!(!at(s, s.pointer_location), "not lit, not a target");
        s.pointer_button(0x110, ButtonState::Pressed, 0);
        assert!(!s.trusted_ui.is_open());
        s.pointer_button(0x110, ButtonState::Released, 0);

        s.capture_seen = Some(Instant::now());
        assert!(at(s, s.pointer_location));
        s.pointer_button(0x111, ButtonState::Pressed, 0);
        assert!(!s.trusted_ui.is_open(), "only the left button opens it");
        assert_ne!(s.trusted_ui.marks.pressed, 0);
        s.pointer_location = (0.0, 500.0).into();
        s.pointer_button(0x111, ButtonState::Released, 0);
        assert_eq!(
            s.trusted_ui.marks.pressed, 0,
            "its release is swallowed, off the marker too"
        );
        s.pointer_location = (geo.loc + r.loc + Point::from((r.size.w / 2, r.size.h / 2))).to_f64();
        s.pointer_button(0x110, ButtonState::Pressed, 0);
        assert!(s.trusted_ui.is_open(), "the panel is up");
        assert!(super::super::panel::owns(s, s.trusted_ui.token().unwrap()));
    }

    /// On a rotated output the marker and the pills stay in its top-right
    /// corner, the pills under the marker.
    #[test]
    fn a_rotated_output_keeps_both_in_its_corner() {
        use smithay::output::{Mode, PhysicalProperties, Subpixel};
        use smithay::utils::Transform;
        let o = Output::new(
            "rot".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "t".into(),
                model: "t".into(),
            },
        );
        let mode = Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        };
        o.change_current_state(Some(mode), Some(Transform::_90), None, None);
        let logical = super::super::logical_size(&o).unwrap();
        let m = crate::render::capture::indicator_rect(&o).unwrap();
        assert!(m.loc.x + m.size.w <= logical.w, "the marker is on screen");
        let s = Shown {
            agent: 1,
            name: "agent:a".into(),
            window: None,
            secs: 10,
            held: 0,
        };
        let p = pill_rect(&s, 0, logical);
        assert!(p.loc.y >= m.loc.y + m.size.h);
        assert_eq!(p.loc.x + p.size.w, m.loc.x + m.size.w, "right edges align");
    }

    #[test]
    fn pills_stack_under_the_capture_marker_and_stay_on_screen() {
        let s = Shown {
            agent: 1,
            name: "agent:a".into(),
            window: None,
            secs: 10,
            held: 0,
        };
        let logical = Size::from((1920, 1080));
        let a = pill_rect(&s, 0, logical);
        let b = pill_rect(&s, 1, logical);
        assert!(a.loc.y >= TOP && a.loc.x + a.size.w <= logical.w);
        assert_eq!(b.loc.y, a.loc.y + PILL_H + PILL_GAP);
    }
}
