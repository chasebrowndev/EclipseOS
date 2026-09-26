// SPDX-License-Identifier: AGPL-3.0-only
//! Trusted UI (COMP-10). TCB: every line here gets owner line-by-line review.
//!
//! A prompt is compositor-drawn and compositor-answered. No client asks for
//! one, draws one or sees one:
//!
//! * **Seat.** While a prompt is up it takes the human seat's keyboard and
//!   pointer (§4). Clients receive neither, and `keyboard_shortcuts_inhibit`
//!   does not apply, because the filter runs before any client does. The
//!   override chord still works (COMP-04 §6). Agent seats are not touched.
//! * **Render.** The backends splice it in front-most, above the capture
//!   indicator, and never into a capture target: `render/capture.rs` does
//!   not name this module (a test checks that).
//! * **Lock.** Under the session lock a prompt is neither drawn nor holding
//!   the seat. A prompt that can grant must not be answerable by whoever is
//!   at a locked machine. It stays pending and comes back on unlock.
//! * **Timeout.** Unanswered, it resolves to its `Safe` button (§3.2: fails
//!   closed).
//!
//! One prompt at a time. `open` refuses a second, and the owner re-queues it.

pub mod approval;
pub mod modal;

use std::{collections::BTreeMap, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                solid::{SolidColorBuffer, SolidColorRenderElement},
                texture::{TextureBuffer, TextureRenderElement},
                Kind,
            },
            gles::{GlesRenderer, GlesTexture},
            ImportMem,
        },
    },
    input::{keyboard::Keysym, pointer::MotionEvent},
    output::Output,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point, Rectangle, Scale, Size, Transform, SERIAL_COUNTER},
};

use crate::{render::AbyssRenderElement, state::AbyssState};
pub use modal::{Button, Modal, Role};

/// §3.2: a prompt nobody answers fails closed after this long.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// Everything behind a prompt is dimmed, on every output, so it is plain that
/// the session is waiting on the human and nothing behind it takes input.
const DIM: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const DIM_ALPHA: f32 = 0.6;

/// The answer to a prompt, handed to [`resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub token: u64,
    pub button: usize,
    pub role: Role,
}

#[derive(Debug)]
struct Open {
    modal: modal::Modal,
    layout: modal::Layout,
    focus: usize,
    /// The button a pointer press went down on. A click counts only if the
    /// release lands on the same one, so a press already in flight when the
    /// prompt appeared cannot answer it by being let go.
    pressed: Option<usize>,
}

/// Trusted-UI state, owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct TrustedUi {
    open: Option<Open>,
    /// The uploaded panel per device scale, with the focus it was drawn with.
    /// Kept across frames so the damage tracker sees the same element.
    art: BTreeMap<usize, (usize, TextureBuffer<GlesTexture>)>,
    /// One dim buffer per output size, for the same reason.
    dim: BTreeMap<(i32, i32), SolidColorBuffer>,
    /// The command-approval prompt's widget, if that is what is up.
    asking: approval::Asking,
}

impl TrustedUi {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The token of the prompt that is up, if any.
    pub fn token(&self) -> Option<u64> {
        self.open.as_ref().map(|o| o.modal.token)
    }
}

/// Whether a prompt holds the human seat right now. False under the lock.
pub fn holds_seat(state: &AbyssState) -> bool {
    state.trusted_ui.is_open() && !state.lock.locked
}

/// Put a prompt up. `false`, and nothing changes, if one is already up.
pub fn open(state: &mut AbyssState, modal: modal::Modal) -> bool {
    if state.trusted_ui.is_open() {
        return false;
    }
    let token = modal.token;
    let focus = modal.safe();
    let layout = modal::layout(&modal);
    state.trusted_ui.open = Some(Open {
        modal,
        layout,
        focus,
        pressed: None,
    });
    state.trusted_ui.art.clear();
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(TIMEOUT), move |_, _, state| {
            if state.trusted_ui.token() == Some(token) {
                tracing::info!(token, "trusted prompt timed out; resolving to its safe button");
                choose_safe(state);
            }
            TimeoutAction::Drop
        });
    if let Err(e) = timer {
        // No timeout means no fail-closed path; refuse rather than risk a
        // prompt that holds the seat forever.
        tracing::error!(?e, "trusted prompt timer; prompt refused");
        state.trusted_ui.open = None;
        return false;
    }
    release_pointer_focus(state);
    crate::backend::damage_all(state);
    true
}

/// No client keeps pointer focus behind a prompt: it would otherwise still
/// be hovered, and see the leave only when the prompt was gone.
pub fn release_pointer_focus(state: &mut AbyssState) {
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let location = state.pointer_location;
    pointer.motion(
        state,
        None,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time: state.start_time.elapsed().as_millis() as u32,
        },
    );
    pointer.frame(state);
    state.last_pointer_focus = None;
}

/// A key while the prompt holds the seat.
pub fn key(state: &mut AbyssState, sym: Keysym) {
    let Some(o) = state.trusted_ui.open.as_mut() else {
        return;
    };
    match modal::apply(&o.modal, o.focus, modal::key(sym)) {
        modal::Outcome::Focus(i) => {
            o.focus = i;
            crate::backend::damage_all(state);
        }
        modal::Outcome::Choose(i) => choose(state, i),
        modal::Outcome::Nothing => {}
    }
}

/// A left-button press or release while the prompt holds the seat. Every
/// other button is swallowed by the caller.
pub fn button(state: &mut AbyssState, pressed: bool) {
    let pos = state.pointer_location.to_i32_round::<i32>();
    let panels: Vec<Point<i32, Logical>> = state
        .space
        .outputs()
        .filter_map(|o| panel_origin(state, o))
        .collect();
    let Some(o) = state.trusted_ui.open.as_mut() else {
        return;
    };
    let under = panels
        .iter()
        .find_map(|p| modal::hit(&o.layout, pos.x - p.x, pos.y - p.y));
    if pressed {
        o.pressed = under;
        return;
    }
    if let Some(i) = o.pressed.take().filter(|&p| Some(p) == under) {
        choose(state, i);
    }
}

fn choose_safe(state: &mut AbyssState) {
    if let Some(i) = state.trusted_ui.open.as_ref().map(|o| o.modal.safe()) {
        choose(state, i);
    }
}

/// Close the prompt with `button` as its answer and hand that to its owner.
fn choose(state: &mut AbyssState, button: usize) {
    let Some(o) = state.trusted_ui.open.take() else {
        return;
    };
    state.trusted_ui.art.clear();
    state.trusted_ui.dim.clear();
    let Some(b) = o.modal.buttons().get(button) else {
        return;
    };
    let choice = Choice {
        token: o.modal.token,
        button,
        role: b.role,
    };
    crate::backend::damage_all(state);
    resolve(state, choice);
    // Whatever was waiting behind this prompt gets its turn.
    approval::schedule(state);
    // The pointer is re-evaluated as though it had just moved, so whatever is
    // under it gets its enter now rather than on the next motion.
    state.refresh_pointer_focus();
}

/// Where the answer goes. Owners of prompts are added here.
fn resolve(state: &mut AbyssState, choice: Choice) {
    tracing::debug!(token = choice.token, role = ?choice.role, "trusted prompt answered");
    if approval::owns(state, choice.token) {
        approval::answer(state, choice);
    }
}

/// Take prompt `token` down without an answer: its owner withdrew the
/// question. Nothing is resolved.
fn cancel(state: &mut AbyssState, token: u64) {
    if state.trusted_ui.token() != Some(token) {
        return;
    }
    state.trusted_ui.open = None;
    state.trusted_ui.art.clear();
    state.trusted_ui.dim.clear();
    crate::backend::damage_all(state);
    state.refresh_pointer_focus();
}

/// The panel's top-left on `output`, in global logical coordinates: centred.
fn panel_origin(state: &AbyssState, output: &Output) -> Option<Point<i32, Logical>> {
    let o = state.trusted_ui.open.as_ref()?;
    let geo = state.space.output_geometry(output)?;
    Some(centre(geo, &o.layout))
}

fn centre(geo: Rectangle<i32, Logical>, layout: &modal::Layout) -> Point<i32, Logical> {
    let (w, h) = (layout.w as i32, layout.h as i32);
    (
        geo.loc.x + ((geo.size.w - w) / 2).max(0),
        geo.loc.y + ((geo.size.h - h) / 2).max(0),
    )
        .into()
}

/// The prompt's elements for one output, front-to-back: the panel, then the
/// dim over everything else. Empty with no prompt, or under the lock.
pub fn elements(
    renderer: &mut GlesRenderer,
    ui: &mut TrustedUi,
    locked: bool,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    if locked {
        return Vec::new();
    }
    let Some(o) = ui.open.as_ref() else {
        return Vec::new();
    };
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    let logical: Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    // Whole device pixels, like the annotation cards: the font is a bitmap.
    let dev = (fractional.round() as usize).max(1);

    let mut out = Vec::with_capacity(2);
    if ui.art.get(&dev).map(|(f, _)| *f != o.focus).unwrap_or(true) {
        let raster = modal::rasterize(&o.modal, o.focus, dev);
        match upload(renderer, &raster, dev) {
            Some(buffer) => {
                ui.art.insert(dev, (o.focus, buffer));
            }
            None => {
                // No panel means no way to answer; the dim still holds the
                // seat visibly and the timeout still resolves it.
                tracing::error!("trusted prompt upload failed");
                ui.art.remove(&dev);
            }
        }
    }
    if let Some((_, buffer)) = ui.art.get(&dev) {
        let local = centre(Rectangle::new(output_loc, logical), &o.layout) - output_loc;
        out.push(AbyssRenderElement::Texture(
            TextureRenderElement::from_texture_buffer(
                local.to_f64().to_physical(scale),
                buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ),
        ));
    }
    let dim = ui
        .dim
        .entry((logical.w, logical.h))
        .or_insert_with(|| SolidColorBuffer::new(logical, DIM));
    out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        dim,
        Point::from((0, 0)),
        scale,
        DIM_ALPHA,
        Kind::Unspecified,
    )));
    out
}

fn upload(
    renderer: &mut GlesRenderer,
    raster: &crate::render::text::Raster,
    bs: usize,
) -> Option<TextureBuffer<GlesTexture>> {
    let texture: GlesTexture = renderer
        .import_memory(&raster.px, Fourcc::Abgr8888, (raster.w, raster.h).into(), false)
        .ok()?;
    Some(TextureBuffer::from_texture(
        renderer,
        texture,
        bs as i32,
        Transform::Normal,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_pass_cannot_see_trusted_ui() {
        let src = include_str!("../render/capture.rs");
        assert!(
            !src.contains("trusted_ui"),
            "capture.rs names trusted_ui; a prompt must never reach a capture target"
        );
    }

    #[test]
    fn prompts_are_front_most_in_every_backend() {
        for (src, path, top_first) in [
            (include_str!("../backend/drm.rs"), "drm.rs", true),
            (include_str!("../backend/winit.rs"), "winit.rs", false),
            (include_str!("../backend/headless.rs"), "headless.rs", false),
        ] {
            let ui = src
                .find("trusted_ui::elements")
                .unwrap_or_else(|| panic!("{path} does not draw trusted prompts"));
            let ind = src
                .find("capture::indicator")
                .unwrap_or_else(|| panic!("{path} does not draw the indicator"));
            // drm appends top-first; winit and headless splice each pass in
            // at index 0, which reverses source order.
            assert_eq!(ui < ind, top_first, "{path}: a prompt is behind the indicator");
        }
    }

    #[test]
    fn the_panel_is_centred_and_never_off_the_top_left() {
        let m = Modal::new(
            1,
            "h",
            None,
            "b",
            "l",
            "x",
            vec![Button {
                label: "Not now",
                role: Role::Safe,
            }],
        )
        .unwrap();
        let l = modal::layout(&m);
        let geo = Rectangle::new(Point::from((1920, 0)), Size::from((1920, 1080)));
        let p = centre(geo, &l);
        assert_eq!(p.x, 1920 + (1920 - l.w as i32) / 2);
        let tiny = Rectangle::new(Point::from((0, 0)), Size::from((100, 100)));
        assert_eq!(centre(tiny, &l), Point::from((0, 0)));
    }
}
