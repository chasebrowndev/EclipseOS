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
//!   at a locked machine. It stays pending and comes back on unlock, unless
//!   the timeout runs out first.
//! * **Timeout.** Unanswered, it resolves to its `Safe` button (§3.2: fails
//!   closed). The clock runs under the lock too.
//! * **Arming.** For [`ARM`] after it appears, only Escape counts. A key or
//!   click already on its way (a `<Tab><space>` typed into a shell, a double
//!   click) cannot answer a prompt the human has not seen (§3.2, §7).
//!
//! One prompt at a time. `open` refuses a second, and the owner re-queues it.
//!
//! Owners: command-widget approval ([`approval`], §3.11, ADR 0067), the
//! destructive-system-action confirmation ([`erase`], §3.10, ADR 0061),
//! asked for over the root-only [`socket`], the agent consent prompt
//! ([`consent`], §3.2) and personal-secret entry ([`phrase`], §2).
//!
//! Every prompt shows the personal secret in its fixed frame at the top, or
//! the unconfigured warning while there is none (§2).

pub mod approval;
pub mod batch;
pub mod consent;
pub mod erase;
pub mod install;
pub mod modal;
pub mod notice;
pub mod panel;
pub mod phrase;
pub mod queue;
pub mod slot;
pub mod socket;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

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

/// How long a new prompt ignores everything but Escape.
pub const ARM: Duration = Duration::from_millis(750);

/// Everything behind a prompt is dimmed, on every output, so it is plain that
/// the session is waiting on the human and nothing behind it takes input.
const DIM: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const DIM_ALPHA: f32 = 0.6;

/// The answer to a prompt, handed to [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub token: u64,
    pub button: usize,
    pub role: Role,
    /// The prompt's typed text, when it took any.
    pub typed: Option<modal::Typed>,
    /// Nobody answered: the timeout picked the `Safe` button. Owners that
    /// report a timeout differently from a refusal (§3.2 `prompt_timeout`)
    /// read this.
    pub timed_out: bool,
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
    /// When it went up; input before `shown + ARM` is ignored.
    shown: Instant,
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
    /// The erase prompt's requester, if that is what is up.
    erase: erase::State,
    /// The personal secret (§2), `None` until the owner sets one.
    pub phrase: Option<phrase::Phrase>,
    /// Phrase entry, if that is what is up.
    entering: phrase::Entering,
    /// Agent requests waiting on the human (§3.2, COMP-11 §4).
    pub consent: consent::Queue,
    /// The emergency panel's page, while it is up (§3.3).
    pub(crate) panel: panel::Panel,
    /// Batch prompts waiting or shown (§3.7).
    pub(crate) batches: batch::Batches,
    /// Agent install reviews waiting or shown (A-07 §3).
    pub(crate) installs: install::Installs,
    /// The bound trusted socket, so a clean exit can unlink it.
    pub path: Option<PathBuf>,
}

impl TrustedUi {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Whether a prompt is up or pending, locked or not. The injection and
    /// client-focus paths refuse while it is: nothing scripted may steer
    /// around a prompt, even one the lock is hiding.
    pub fn active(&self) -> bool {
        self.is_open()
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
        shown: Instant::now(),
    });
    state.trusted_ui.art.clear();
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(TIMEOUT), move |_, _, state| {
            if state.trusted_ui.token() == Some(token) {
                tracing::info!(token, "trusted prompt timed out; resolving to its safe button");
                choose_safe(state, true);
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

/// Tests only: let the prompt that is up take input now.
#[cfg(test)]
pub(crate) fn arm_now(state: &mut AbyssState) {
    if let Some(o) = state.trusted_ui.open.as_mut() {
        o.shown = Instant::now() - ARM;
    }
}

/// No client keeps pointer focus behind a prompt: it would otherwise still
/// be hovered, and see the leave only when the prompt was gone.
///
/// A button held as the prompt opens has put a grab on the pointer (a click
/// grab keeps focus on the pressed surface whatever the motion says), so the
/// grab is ended too. Otherwise that client would keep getting motion
/// until the release (COMP-10 §4).
///
/// The motion to nothing comes first: ending a grab restores focus to the
/// pending one, and ending a drag drops it on its current target. Moved to
/// nothing, the click grab restores to no surface and the drag is cancelled.
pub fn release_pointer_focus(state: &mut AbyssState) {
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    let time = state.start_time.elapsed().as_millis() as u32;
    let location = state.pointer_location;
    pointer.motion(
        state,
        None,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    if pointer.is_grabbed() {
        pointer.unset_grab(state, SERIAL_COUNTER.next_serial(), time);
    }
    pointer.frame(state);
    state.last_pointer_focus = None;
}

/// A key while the prompt holds the seat.
pub fn key(state: &mut AbyssState, sym: Keysym) {
    let Some(o) = state.trusted_ui.open.as_mut() else {
        return;
    };
    let k = modal::key(sym, o.modal.takes_text());
    if k != modal::Key::Escape && o.shown.elapsed() < ARM {
        return;
    }
    match modal::apply(&mut o.modal, o.focus, k) {
        modal::Outcome::Focus(i) => {
            o.focus = i;
            crate::backend::damage_all(state);
        }
        modal::Outcome::Edited => {
            // The cached panel is keyed by focus only; the text changed.
            state.trusted_ui.art.clear();
            crate::backend::damage_all(state);
        }
        modal::Outcome::Choose(i) => choose(state, i, false),
        modal::Outcome::Nothing => {}
    }
}

/// A left-button press or release while the prompt holds the seat. Every
/// other button is swallowed by the caller.
pub fn button(state: &mut AbyssState, pressed: bool) {
    let pos = state.pointer_location.to_i32_round::<i32>();
    let under = state
        .space
        .outputs()
        .filter_map(|out| panel_origin(state, out))
        .find_map(|p| {
            let o = state.trusted_ui.open.as_ref()?;
            modal::hit(&o.layout, pos.x - p.x, pos.y - p.y)
        });
    let Some(o) = state.trusted_ui.open.as_mut() else {
        return;
    };
    // §3.10: the erase prompt is keyboard-only. A click aimed at it is dropped.
    if o.shown.elapsed() < ARM || o.modal.token >= erase::TOKEN_BASE {
        o.pressed = None;
        return;
    }
    if pressed {
        o.pressed = under;
        return;
    }
    if let Some(i) = o.pressed.take().filter(|&p| Some(p) == under) {
        choose(state, i, false);
    }
}

fn choose_safe(state: &mut AbyssState, timed_out: bool) {
    if let Some(i) = state.trusted_ui.open.as_ref().map(|o| o.modal.safe()) {
        choose(state, i, timed_out);
    }
}

/// Close the prompt with `button` as its answer and hand that to its owner.
fn choose(state: &mut AbyssState, button: usize, timed_out: bool) {
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
        typed: o.modal.typed().cloned(),
        timed_out,
    };
    crate::backend::damage_all(state);
    resolve(state, choice);
    // Whatever was waiting behind this prompt gets its turn.
    approval::schedule(state);
    consent::schedule(state);
    batch::schedule(state);
    install::schedule(state);
    // The pointer is re-evaluated as though it had just moved, so whatever is
    // under it gets its enter now rather than on the next motion.
    state.refresh_pointer_focus();
}

/// Where the answer goes. Owners of prompts are added here.
fn resolve(state: &mut AbyssState, choice: Choice) {
    tracing::debug!(token = choice.token, role = ?choice.role, "trusted prompt answered");
    if approval::owns(state, choice.token) {
        approval::answer(state, choice);
    } else if erase::owns(state, choice.token) {
        erase::answer(state, choice);
    } else if phrase::owns(state, choice.token) {
        phrase::answer(state, choice);
    } else if consent::owns(state, choice.token) {
        consent::answer(state, choice);
    } else if batch::owns(state, choice.token) {
        batch::answer(state, choice);
    } else if panel::owns(state, choice.token) {
        panel::answer(state, choice);
    } else if install::owns(state, choice.token) {
        install::answer(state, choice);
    } else if notice::owns(choice.token) {
        // Acknowledged; nothing follows from a notice.
    }
}

/// Swap the prompt that is up for `modal`, if `modal` carries the same
/// token: its owner learned more (the panel's audit tail arrived). Focus is
/// kept where it was, or falls back to the safe button; arming is not reset,
/// because the human is already looking at it.
pub(crate) fn replace(state: &mut AbyssState, modal: modal::Modal) -> bool {
    let Some(o) = state.trusted_ui.open.as_mut() else {
        return false;
    };
    if o.modal.token != modal.token {
        return false;
    }
    let focus = if o.focus < modal.buttons().len() {
        o.focus
    } else {
        modal.safe()
    };
    o.layout = modal::layout(&modal);
    o.modal = modal;
    o.focus = focus;
    o.pressed = None;
    state.trusted_ui.art.clear();
    crate::backend::damage_all(state);
    true
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
    let Some(logical) = logical_size(output) else {
        return Vec::new();
    };
    let fractional = output.current_scale().fractional_scale();
    let scale = Scale::from(fractional);
    // Whole device pixels, like the annotation cards: the font is a bitmap.
    let dev = (fractional.round() as usize).max(1);

    let mut out = Vec::with_capacity(2);
    if ui.art.get(&dev).map(|(f, _)| *f != o.focus).unwrap_or(true) {
        let raster = modal::rasterize(
            &o.modal,
            o.focus,
            dev,
            ui.phrase.as_ref().map(phrase::Phrase::as_str),
        );
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

/// The output's logical size, transformed as `space.output_geometry` is:
/// the hit test in `button` centres on that, and the drawn panel must sit
/// where it hits, rotated outputs included.
fn logical_size(output: &Output) -> Option<Size<i32, Logical>> {
    let mode = output.current_mode()?;
    Some(
        output
            .current_transform()
            .transform_size(mode.size)
            .to_f64()
            .to_logical(output.current_scale().fractional_scale())
            .to_i32_round(),
    )
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

    /// A button held down as the prompt opens leaves no client grab behind:
    /// the pressed surface must not keep the pointer while the prompt is up.
    #[test]
    fn a_held_button_does_not_keep_a_client_grab_under_a_prompt() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        s.inject_pointer_button(0x110, true, 0);
        let pointer = s.seat.get_pointer().unwrap();
        assert!(pointer.is_grabbed(), "a press starts a click grab");
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
        assert!(open(s, m));
        assert!(!pointer.is_grabbed(), "the prompt ended the grab");
    }

    #[test]
    fn a_rotated_output_is_drawn_where_it_is_hit() {
        use smithay::output::{Mode, PhysicalProperties, Subpixel};
        let o = Output::new(
            "t".into(),
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
        assert_eq!(logical_size(&o), Some(Size::from((1080, 1920))));
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

/// Not a check: renders every trusted surface to `$ECLIPSE_DUMP_DIR/*.pam`
/// for a visual review (`cargo test -p ec-abyss dump_trusted_surfaces --
/// --ignored`). PAM is RGBA with no dependency; `convert x.pam x.png` turns
/// it into a PNG.
#[cfg(test)]
#[test]
#[ignore]
fn dump_trusted_surfaces() {
    use std::io::Write;
    let Some(dir) = std::env::var_os("ECLIPSE_DUMP_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, m: &modal::Modal, phrase: Option<&str>| {
        let r = modal::rasterize(m, m.safe(), 2, phrase);
        let mut f = std::fs::File::create(dir.join(format!("{name}.pam"))).unwrap();
        write!(
            f,
            "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
            r.w, r.h
        )
        .unwrap();
        f.write_all(&r.px).unwrap();
    };
    let ask = consent::Ask {
        principal: "agent:research-7".into(),
        action: "Click \"Send\"".into(),
        app: "Gmail - Firefox".into(),
        window: "Compose: Q3 invoice".into(),
        task: "Summarize this week's invoices".into(),
        category: Some(("communication.send".into(), consent::Reversal::None)),
        untrusted_source: Some("acme-invoices.com (web page)".into()),
        task_scope: "click handle:4".into(),
        unattended_scope: "click app_id:org.mozilla.firefox".into(),
        note: "checking the Q3 total before sending".into(),
    };
    write(
        "consent",
        &consent::modal(1, &ask).unwrap(),
        Some("blue heron 42"),
    );
    let mut routine = ask.clone();
    routine.category = None;
    routine.untrusted_source = None;
    write("consent-routine", &consent::modal(1, &routine).unwrap(), None);
    let mut h = crate::shell::focus::state_tests::harness();
    phrase::prompt_if_unset(&mut h.state);
    if let Some(o) = h.state.trusted_ui.open.as_ref() {
        write("phrase-entry", &o.modal, None);
    }
}
