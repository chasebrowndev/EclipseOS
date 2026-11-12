// SPDX-License-Identifier: AGPL-3.0-only
//! The capture indicator's eye (COMP-10 §3.6): the taskbar's eclipse eye with
//! an animated pupil, drawn by the compositor over the spot where the bar
//! draws its launcher ring, bar running or not.
//!
//! State-free like the rest of this crate: the pupil is a pure function of
//! the time since capture began ([`pupil`]), with no RNG and no per-instance
//! state, so every output shows the same glance. The textures are cached by
//! the caller in [`EyeTextures`]; the frame is pushed with [`elements`].
//! Geometry and timing come from [`ec_theme::bar_mark`], which a test in
//! `ec-ui` pins to the bar's own tokens.
//!
//! The eye is flattened onto the theme's base colour: opaque, so the ring a
//! running bar draws beneath it never shows through and the eye looks the
//! same with no bar at all, and so a translucent theme cannot fade it.

use std::time::{Duration, Instant};

use ec_theme::bar_mark as m;
use ec_theme::Palette;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ContextId, Renderer};
use smithay::utils::{Logical, Point, Size};

use crate::text::Rgba;
use crate::AbyssRenderElement;

/// The iris's radius: the corona's outer edge (`eye::outer()` in the bar).
const OUTER: f32 = m::EYE_DISC / 2.0;
/// Frame interval while the pupil moves.
const FRAME: Duration = Duration::from_millis(m::EYE_FRAME_MS);
/// One glance slot: a hold of up to the maximum, then a dart. Fixed, so the
/// pupil at time `t` needs no walk through the slots before it.
const SLOT_MS: u64 = m::EYE_HOLD_MAX_MS + m::EYE_DART_MS;
/// The pupil opens in this many discrete sizes (one cached texture each).
const STEPS: usize = 8;

/// Where the pupil is, as fractions of the iris radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pupil {
    /// Pupil centre relative to the iris centre.
    pub offset: (f32, f32),
    /// Pupil radius.
    pub radius: f32,
}

/// The three colours of one eye, straight RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colours {
    pub iris: Rgba,
    pub rim: Rgba,
    pub pupil: Rgba,
}

impl Colours {
    /// Iris = the theme's `eye_iris`, rim = `text_secondary` (the bar's
    /// resting ring), both flattened onto `base`; the pupil is the configured
    /// colour with alpha forced to 1.
    pub fn from_palette(p: &Palette, pupil: Rgba) -> Colours {
        Colours {
            iris: flatten(p.eye_iris.0, p.base.0),
            rim: flatten(p.text_secondary.0, p.base.0),
            pupil: [pupil[0], pupil[1], pupil[2], 1.0],
        }
    }
}

fn flatten(c: Rgba, base: Rgba) -> Rgba {
    let a = c[3].clamp(0.0, 1.0);
    let f = |i: usize| c[i] * a + base[i] * (1.0 - a);
    [f(0), f(1), f(2), 1.0]
}

/// The eye's centre, output-local logical: the middle of the launcher ring.
///
/// The bar's eye surface is the launcher button's own `TASK_MIN x TASK_H` box
/// at `gaps_x + EDGE` from the left and `gaps_y + (PILL_H - TASK_H) / 2` from
/// the bar's edge (cast to whole pixels, as the bar does); the ring is centred
/// in it. A bottom bar measures up from the bottom edge.
pub fn centre(output: Size<f64, Logical>, bottom: bool, gaps_x: f64, gaps_y: f64) -> Point<f64, Logical> {
    let x = gaps_x + f64::from(m::EDGE) + f64::from(m::TASK_MIN) / 2.0;
    let inset = f64::from(((m::PILL_H - m::TASK_H) / 2.0) as i32);
    let y = gaps_y + inset + f64::from(m::TASK_H) / 2.0;
    Point::from((x, if bottom { output.h - y } else { y }))
}

fn mix(mut x: u64) -> u64 {
    // splitmix64 finaliser.
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Uniform in `0..1` from a hash.
fn unit(h: u64) -> f32 {
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// How long slot `i` holds before it darts.
fn hold_ms(i: u64) -> u64 {
    m::EYE_HOLD_MIN_MS + mix(i.wrapping_mul(3)) % (m::EYE_HOLD_MAX_MS - m::EYE_HOLD_MIN_MS + 1)
}

/// Where slot `i` looks, as fractions of the iris radius. The slot before the
/// first looks straight ahead.
fn target(i: Option<u64>) -> (f32, f32) {
    let Some(i) = i else { return (0.0, 0.0) };
    if mix(i.wrapping_mul(3) + 1) % m::EYE_RECENTRE == 0 {
        return (0.0, 0.0);
    }
    let h = mix(i.wrapping_mul(3) + 2);
    let angle = unit(h) * std::f32::consts::TAU;
    let span = 1.0 - m::EYE_GLANCE;
    let dist = m::EYE_WANDER * (m::EYE_GLANCE + span * unit(mix(h)));
    (dist * angle.cos(), dist * angle.sin())
}

/// A saccade: fast off the mark, settling onto the target.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// The pupil opening: no snap at either end.
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// The pupil `since` capture began: opens from nothing to `EYE_PUPIL` over
/// `EYE_RESIZE_MS`, then holds and darts between hashed targets.
pub fn pupil(since: Duration) -> Pupil {
    let ms = since.as_secs_f32() * 1000.0;
    let open = (ms / m::EYE_RESIZE_MS as f32).clamp(0.0, 1.0);
    let radius = m::EYE_PUPIL * ease_in_out(open);
    let t = since.as_millis() as u64;
    if t < m::EYE_RESIZE_MS {
        return Pupil {
            offset: (0.0, 0.0),
            radius,
        };
    }
    let into = since - Duration::from_millis(m::EYE_RESIZE_MS);
    let slot = (into.as_millis() as u64) / SLOT_MS;
    let local = into.as_secs_f32() * 1000.0 - (slot * SLOT_MS) as f32;
    let hold = hold_ms(slot) as f32;
    let from = target(slot.checked_sub(1));
    let to = target(Some(slot));
    let k = ease_out(((local - hold) / m::EYE_DART_MS as f32).clamp(0.0, 1.0));
    Pupil {
        offset: (from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k),
        radius,
    }
}

/// How long until the pupil next needs a frame: a frame interval while it
/// opens or darts, else the time to the next dart. The compositor renders
/// only at these instants, so a held eye costs nothing.
pub fn next_frame(since: Duration) -> Duration {
    let t = since.as_millis() as u64;
    if t < m::EYE_RESIZE_MS {
        // The last opening frame lands on the end, not past it.
        return FRAME.min(Duration::from_millis(m::EYE_RESIZE_MS - t).max(Duration::from_millis(1)));
    }
    let into = (since - Duration::from_millis(m::EYE_RESIZE_MS)).as_millis() as u64;
    let (slot, local) = (into / SLOT_MS, into % SLOT_MS);
    let hold = hold_ms(slot);
    let dart_end = hold + m::EYE_DART_MS;
    let ms = if local < hold {
        hold - local
    } else if local < dart_end {
        (dart_end - local).min(m::EYE_FRAME_MS)
    } else {
        SLOT_MS - local + hold_ms(slot + 1)
    };
    Duration::from_millis(ms.max(1))
}

/// Whole logical pixels of the eye's texture box: even, so its centre is a
/// pixel corner and an integer ring centre keeps the eye on the pixel grid.
/// A test checks it holds the art (`2 * (OUTER + EYE_BLEED)` plus a pixel's margin).
const EYE_BOX: i32 = 22;
/// The pupil's texture box, likewise.
const PUPIL_BOX: i32 = 10;

struct Entry {
    ctx: ContextId<GlesTexture>,
    bs: i32,
    colours: Colours,
    eye: TextureBuffer<GlesTexture>,
    /// Index `k` is the pupil at `k / STEPS` of its full size; `0` is empty.
    pupils: Vec<Option<TextureBuffer<GlesTexture>>>,
}

/// What the compositor keeps for the eye: its textures, when this capture
/// began, and whether a wake-up timer is armed.
#[derive(Default)]
pub struct EyeState {
    pub textures: EyeTextures,
    /// When the indicator first lit for the current capture; `None` while dark.
    pub started: Option<Instant>,
    /// A timer for the next frame is armed.
    pub ticking: bool,
}

impl EyeState {
    /// Time since the indicator lit, starting the clock if this is the first
    /// lit frame. `None` (and the clock reset) while capture is not active, so
    /// the next capture opens the pupil afresh.
    pub fn since(&mut self, active: bool, now: Instant) -> Option<Duration> {
        if !active {
            self.started = None;
            return None;
        }
        Some(now.saturating_duration_since(*self.started.get_or_insert(now)))
    }
}

/// The eye's textures, cached per renderer and raster scale and rebuilt only
/// when a colour changes.
#[derive(Default)]
pub struct EyeTextures {
    entries: Vec<Entry>,
}

/// Antialiased coverage of a disc of radius `r` at distance `d`, in pixels.
fn cover(d: f32, r: f32) -> f32 {
    (r - d + 0.5).clamp(0.0, 1.0)
}

fn premul(c: Rgba, k: f32) -> [f32; 4] {
    [c[0] * k, c[1] * k, c[2] * k, k]
}

fn raster(box_px: i32, bs: i32, shade: impl Fn(f32) -> [f32; 4]) -> crate::text::Raster {
    let n = (box_px * bs) as usize;
    let mid = n as f32 / 2.0;
    let mut px = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 + 0.5 - mid).powi(2) + (y as f32 + 0.5 - mid).powi(2)).sqrt() / bs as f32;
            for v in shade(d) {
                px.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
    }
    crate::text::Raster {
        w: n as i32,
        h: n as i32,
        px,
    }
}

fn build(renderer: &mut GlesRenderer, bs: i32, c: Colours) -> Option<Entry> {
    let inner = OUTER - m::EYE_RIM - m::EYE_BLEED;
    let edge = OUTER + m::EYE_BLEED;
    // The glass inside the rim and the rim as its own band, so the two never
    // overlap and double the coverage at the seam (as the bar draws it).
    let eye = raster(EYE_BOX, bs, |d| {
        let (i, e) = (cover(d, inner), cover(d, edge));
        let (g, r) = (premul(c.iris, i), premul(c.rim, (e - i).max(0.0)));
        [g[0] + r[0], g[1] + r[1], g[2] + r[2], g[3] + r[3]]
    });
    let eye = crate::annotation::upload(renderer, &eye, bs as usize)?;
    let mut pupils = vec![None];
    for k in 1..=STEPS {
        let r = OUTER * m::EYE_PUPIL * k as f32 / STEPS as f32;
        let ras = raster(PUPIL_BOX, bs, |d| premul(c.pupil, cover(d, r)));
        pupils.push(Some(crate::annotation::upload(renderer, &ras, bs as usize)?));
    }
    Some(Entry {
        ctx: renderer.context_id(),
        bs,
        colours: c,
        eye,
        pupils,
    })
}

/// Push the eye, front-to-back (pupil, then the iris and rim), centred on
/// `centre` (output-local logical). `scale` is the output's fractional scale.
/// Nothing but the pushes allocates, and only on a cache miss.
pub fn elements(
    textures: &mut EyeTextures,
    renderer: &mut GlesRenderer,
    centre: Point<f64, Logical>,
    scale: f64,
    colours: Colours,
    pupil: Pupil,
    out: &mut Vec<AbyssRenderElement>,
) {
    let bs = (scale.ceil() as i32).max(1);
    let ctx = renderer.context_id();
    let at = textures.entries.iter().position(|e| e.bs == bs && e.ctx == ctx);
    let at = match at {
        Some(i) if textures.entries[i].colours == colours => i,
        found => {
            let Some(entry) = build(renderer, bs, colours) else {
                return;
            };
            match found {
                Some(i) => {
                    textures.entries[i] = entry;
                    i
                }
                None => {
                    textures.entries.push(entry);
                    textures.entries.len() - 1
                }
            }
        }
    };
    let e = &textures.entries[at];
    let to_phys = |p: Point<f64, Logical>| p.to_physical(scale);
    let step = ((pupil.radius / m::EYE_PUPIL) * STEPS as f32).round() as usize;
    if let Some(Some(buf)) = e.pupils.get(step.min(STEPS)) {
        let half = f64::from(PUPIL_BOX) / 2.0;
        let c = Point::<f64, Logical>::from((
            centre.x + f64::from(pupil.offset.0 * OUTER),
            centre.y + f64::from(pupil.offset.1 * OUTER),
        ));
        out.push(AbyssRenderElement::Texture(
            TextureRenderElement::from_texture_buffer(
                to_phys(Point::from((c.x - half, c.y - half))),
                buf,
                None,
                None,
                None,
                Kind::Unspecified,
            ),
        ));
    }
    let half = f64::from(EYE_BOX) / 2.0;
    out.push(AbyssRenderElement::Texture(
        TextureRenderElement::from_texture_buffer(
            to_phys(Point::from((centre.x - half, centre.y - half))),
            &e.eye,
            None,
            None,
            None,
            Kind::Unspecified,
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn centre_follows_the_bars_ring() {
        let out = Size::<f64, Logical>::from((1920.0, 1080.0));
        // Top: gaps-out + EDGE + TASK_MIN/2 across, gaps-out-vertical + PILL_H/2 down.
        let c = centre(out, false, 12.0, 6.0);
        assert_eq!((c.x, c.y), (12.0 + 8.0 + 17.0, 6.0 + 20.0));
        // Bottom: the same distance up from the bottom edge.
        let c = centre(out, true, 12.0, 6.0);
        assert_eq!((c.x, c.y), (37.0, 1080.0 - 26.0));
    }

    #[test]
    fn pupil_stays_inside_the_rim_and_is_deterministic() {
        let reach = m::EYE_PUPIL + m::EYE_WANDER;
        for t in (0..60_000).step_by(7) {
            let p = pupil(ms(t));
            let d = (p.offset.0.powi(2) + p.offset.1.powi(2)).sqrt();
            assert!(d <= m::EYE_WANDER + 1e-4, "{t}: {d}");
            assert!(p.radius + d <= reach + 1e-4, "{t}");
            assert_eq!(p, pupil(ms(t)));
        }
        // The far edge clears the rim's inner edge, as in the bar.
        assert!(OUTER * reach <= OUTER - m::EYE_RIM - m::EYE_BLEED);
    }

    #[test]
    fn the_pupil_opens_from_nothing() {
        assert_eq!(pupil(ms(0)).radius, 0.0);
        let half = pupil(ms(m::EYE_RESIZE_MS / 2)).radius;
        assert!(half > 0.0 && half < m::EYE_PUPIL);
        assert_eq!(pupil(ms(m::EYE_RESIZE_MS)).radius, m::EYE_PUPIL);
        assert_eq!(pupil(ms(m::EYE_RESIZE_MS)).offset, (0.0, 0.0));
    }

    #[test]
    fn glances_are_real_glances() {
        for i in 0..200 {
            let (x, y) = target(Some(i));
            let d = (x * x + y * y).sqrt();
            assert!(d == 0.0 || d >= m::EYE_GLANCE * m::EYE_WANDER - 1e-4, "{i}: {d}");
        }
        assert!(
            (0..200).any(|i| target(Some(i)) == (0.0, 0.0)),
            "it recentres now and then"
        );
        assert!((0..200).any(|i| target(Some(i)) != (0.0, 0.0)));
        for i in 0..200 {
            let h = hold_ms(i);
            assert!((m::EYE_HOLD_MIN_MS..=m::EYE_HOLD_MAX_MS).contains(&h));
        }
    }

    #[test]
    fn next_frame_is_fast_only_while_moving() {
        assert_eq!(next_frame(ms(0)), FRAME);
        // The last opening frame is not overshot.
        assert_eq!(next_frame(ms(m::EYE_RESIZE_MS - 4)), ms(4));
        // Holding: sleep right up to the dart, and nothing moves meanwhile.
        let base = m::EYE_RESIZE_MS;
        let hold = hold_ms(0);
        let wait = next_frame(ms(base + 1));
        assert_eq!(wait, ms(hold - 1));
        assert_eq!(pupil(ms(base + 1)), pupil(ms(base + hold - 1)));
        // During the dart: a frame interval, clamped to its end.
        assert_eq!(next_frame(ms(base + hold)), FRAME);
        assert_eq!(next_frame(ms(base + hold + m::EYE_DART_MS - 3)), ms(3));
        // After it: to the next slot's dart.
        let after = next_frame(ms(base + hold + m::EYE_DART_MS));
        assert_eq!(after, ms(SLOT_MS - hold - m::EYE_DART_MS + hold_ms(1)));
        // Following the schedule always makes progress and never skips a dart.
        let mut t = ms(0);
        for _ in 0..2000 {
            let n = next_frame(t);
            assert!(n >= ms(1));
            t += n;
        }
        assert!(t > ms(5000));
    }

    #[test]
    fn the_dart_lands_on_its_target() {
        let base = m::EYE_RESIZE_MS;
        let p = pupil(ms(base + hold_ms(0) + m::EYE_DART_MS));
        let (x, y) = target(Some(0));
        assert!((p.offset.0 - x).abs() < 1e-4 && (p.offset.1 - y).abs() < 1e-4);
    }

    #[test]
    fn boxes_are_even_and_hold_the_art() {
        assert_eq!(EYE_BOX % 2, 0);
        assert_eq!(PUPIL_BOX % 2, 0);
        assert!(EYE_BOX as f32 >= 2.0 * (OUTER + m::EYE_BLEED) + 1.0);
        assert!(PUPIL_BOX as f32 >= 2.0 * OUTER * m::EYE_PUPIL + 1.0);
    }

    #[test]
    fn colours_are_opaque_and_flattened_onto_the_base() {
        let p = ec_theme::derive(&ec_theme::DARK_SEEDS);
        let c = Colours::from_palette(&p, [1.0, 0.15, 0.15, 0.2]);
        assert_eq!(c.iris[3], 1.0);
        assert_eq!(c.rim[3], 1.0);
        assert_eq!(c.pupil, [1.0, 0.15, 0.15, 1.0]);
    }
}
