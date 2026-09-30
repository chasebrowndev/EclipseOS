// SPDX-License-Identifier: AGPL-3.0-only
//! The one focus path for the human seat (COMP-05 §5, ADR 0042).
//!
//! Keyboard focus is a *function of* the focused output plus that output's
//! workspace focus history. [`decide_pointer_focus`] computes the verdict and
//! is pure — ids and bools in, a [`FocusAction`] out, no `AbyssState`, so the
//! rules are table-testable without a display. [`apply_focus`] is the only
//! place that moves the human seat's keyboard focus, and therefore also the
//! only write site for per-workspace focus history.

use smithay::{
    desktop::Window,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::KeyboardInteractivity,
};

use crate::{
    protocols::standard::seat::KeyboardFocusTarget,
    shell::{arrange, output_of_window, window_surface},
    state::AbyssState,
};

/// What should happen to keyboard focus. Generic only so the decision rules can
/// be unit-tested without standing up a real `Window` (which needs a live
/// toplevel surface), exactly as `workspace::FocusHistory<T>` is; everything
/// outside the tests uses the `Window` default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusAction<W = Window, L = WlSurface> {
    /// Leave focus exactly where it is. Not "refocus what is focused" — a
    /// genuine no-op, so it cannot raise, re-emit or re-order history.
    Keep,
    Window(W),
    /// A click gave the keyboard to a layer surface that accepts it.
    Layer(L),
    /// Nothing focused: an empty workspace on the output the human moved to.
    Clear,
}

/// Why focus moved. Carried only so the journal can say what did it; never
/// logged with any surface content (COMP-05 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusCause {
    Pointer,
    Click,
    Touch,
    Keybind,
    WindowMap,
    WindowUnmap,
    WorkspaceSwitch,
    Ipc,
}

impl FocusCause {
    /// Does focus from this cause also restack the window to the front?
    ///
    /// RAISE-01 raises on focus so a *click* cannot leave a window focused but
    /// behind. Passive causes must not: focus-follows-mouse (`Pointer`) and the
    /// scene-change refresh (`WindowUnmap`, `AbyssState::refresh_pointer_focus`)
    /// move focus without the user asking for a restack, and raising there
    /// rewrites the stacking order under a stationary pointer — which then hides
    /// any window later moved beneath the raised one, since the hit test still
    /// resolves the raised window and pointer focus never re-evaluates.
    fn raises(self) -> bool {
        !matches!(self, FocusCause::Pointer | FocusCause::WindowUnmap)
    }
}

/// The layer surface whose surface is topmost under the pointer.
#[derive(Debug, Clone)]
pub(crate) struct LayerUnder<L = WlSurface> {
    /// Its root surface: what a click hands the keyboard to.
    pub surface: L,
    pub interactivity: KeyboardInteractivity,
    /// It is the layer surface holding the keyboard right now.
    pub holds_keyboard: bool,
}

/// Everything [`decide_pointer_focus`] is allowed to look at. Deliberately
/// plain data: no `&AbyssState`, no smithay handles beyond the window and
/// layer surface types.
#[derive(Debug, Clone)]
pub(crate) struct PointerFocusCtx<W = Window, L = WlSurface> {
    /// Output under the pointer. `None` when the pointer is over no output.
    pub pointer_output: Option<u64>,
    /// Topmost toplevel under the pointer, if any. `None` when a layer
    /// surface stacked above it takes the pointer instead (`layer_under`).
    pub window_under: Option<W>,
    /// Topmost surface under the pointer belongs to this layer surface.
    pub layer_under: Option<LayerUnder<L>>,
    /// The decision is for a button press (or touch-down, or tablet tip),
    /// not pointer motion or a scene change.
    pub click: bool,
    /// The currently focused window, and the output that owns it.
    pub focused: Option<W>,
    pub focused_output: Option<u64>,
    /// Keyboard interactivity of the layer surface holding the keyboard, if a
    /// layer surface holds it at all.
    pub layer_interactivity: Option<KeyboardInteractivity>,
    /// A drag is in flight: a compositor move/resize grab, a client grab on
    /// the seat pointer, a client DnD, or a popup grab. See
    /// [`crate::input::grabs::drag_active`] — the single definition.
    pub drag_active: bool,
    /// A trusted-UI prompt holds the seat (COMP-10 §4).
    pub prompt_grab_active: bool,
    /// `general.focus-follows-mouse-across-outputs`
    pub focus_follows_mouse_across_outputs: bool,
    /// `general.unfocus-on-empty-workspace`
    pub unfocus_on_empty_workspace: bool,
    /// `general.refocus-on-scene-change`. Not consulted by the pointer rules;
    /// it gates the *scene-change* refocus path in
    /// `AbyssState::refresh_pointer_focus`, which reads it off this context.
    pub refocus_on_scene_change: bool,
    /// Focus-history head of the workspace on `pointer_output`.
    pub pointer_output_focus_head: Option<W>,
}

/// Pure. The whole pointer-focus rule set, in precedence order.
pub(crate) fn decide_pointer_focus<W: Clone + PartialEq, L: Clone>(
    ctx: &PointerFocusCtx<W, L>,
) -> FocusAction<W, L> {
    // 1. A trusted-UI prompt owns the seat. First, and above every config key:
    //    configuration must never be able to un-protect a prompt (COMP-10 §4).
    if ctx.prompt_grab_active {
        return FocusAction::Keep;
    }

    // 2. A drag never crosses an output boundary (COMP-05 §5, hard). Focus
    //    freezes at the drag's origin output until the button comes up. This
    //    is a deliberate deviation from Hyprland's unconditional cross-output
    //    FFM (ADR 0042); do not "fix" it.
    if ctx.drag_active && ctx.pointer_output != ctx.focused_output {
        return FocusAction::Keep;
    }

    // 3. A layer surface holding the keyboard blocks FFM. `Exclusive` is the
    //    launcher: letting a toplevel behind it take the keyboard is what made
    //    Escape and typing do nothing — and a click does not take it either
    //    (wlr-layer-shell: the seat "will always give exclusive keyboard focus"
    //    to it).
    //
    //    `OnDemand` is released by a click only, never by hover: the protocol
    //    leaves the mechanism to the compositor and names exactly this one,
    //    "requiring a click even if focus follows the mouse normally". FFM
    //    (COMP-04 §7, COMP-05 §5) is about toplevels; a menu being typed into
    //    must not lose the keyboard to the pointer crossing a window. A click
    //    anywhere but the layer itself moves focus as a click would, so the
    //    layer gets its leave: to another focusable layer surface, to the
    //    window clicked, else back to the workspace's focus head (or nothing).
    match ctx.layer_interactivity {
        Some(KeyboardInteractivity::Exclusive) => return FocusAction::Keep,
        Some(KeyboardInteractivity::OnDemand) => {
            if !ctx.click {
                return FocusAction::Keep;
            }
            if let Some(l) = ctx.layer_under.as_ref() {
                if l.holds_keyboard {
                    return FocusAction::Keep;
                }
                if l.interactivity != KeyboardInteractivity::None {
                    return FocusAction::Layer(l.surface.clone());
                }
            } else if let Some(w) = ctx.window_under.as_ref() {
                // Not "unless already focused": the keyboard is on the layer,
                // whatever `focused` still names.
                return FocusAction::Window(w.clone());
            }
            return match ctx.pointer_output_focus_head.as_ref() {
                Some(head) => FocusAction::Window(head.clone()),
                None => FocusAction::Clear,
            };
        }
        _ => {}
    }

    // 4. A click on a layer surface that accepts the keyboard gives it the
    //    keyboard (`on_demand`'s "click to focus"). Hover never does, and a
    //    click on a `none` surface is empty space to the rules below.
    if let Some(l) = ctx.layer_under.as_ref() {
        if ctx.click && !l.holds_keyboard && l.interactivity != KeyboardInteractivity::None {
            return FocusAction::Layer(l.surface.clone());
        }
    }

    // 5. A window under the pointer takes it, unless it already has it. A
    //    layer surface stacked over the window hides it: hover across a panel
    //    never focuses the toplevel underneath.
    if let Some(w) = ctx.window_under.as_ref().filter(|_| ctx.layer_under.is_none()) {
        if ctx.focused.as_ref() == Some(w) {
            return FocusAction::Keep;
        }
        return FocusAction::Window(w.clone());
    }

    // 6. Nothing has the keyboard, or something that is not a toplevel does —
    //    an on-demand layer surface the human clicked, say. There is no window
    //    focus to move and none to clear, so the empty-space rules below have
    //    nothing to say: clearing here would take the keyboard away from a
    //    layer surface that just earned it (wlcs
    //    LayerSurfaceTest.takes_keyboard_focus_after_click_with_on_demand_*).
    if ctx.focused.is_none() {
        return FocusAction::Keep;
    }

    // 7. The pointer is over no output at all — a gap between monitors, or
    //    past the edge of every one. There is no output to move to, so there
    //    is nothing to decide: dragging the cursor through dead space must
    //    never defocus (it is the exact inverse of the stickiness rule below).
    if ctx.pointer_output.is_none() {
        return FocusAction::Keep;
    }

    // 8. Empty space. On the focused window's own output this changes nothing
    //    — a gap on your own monitor never defocuses (Hyprland's feel).
    if ctx.pointer_output == ctx.focused_output {
        return FocusAction::Keep;
    }
    //    A different output: take that output's focus-history head, else clear.
    if !ctx.focus_follows_mouse_across_outputs {
        return FocusAction::Keep;
    }
    match ctx.pointer_output_focus_head.as_ref() {
        Some(head) if ctx.focused.as_ref() == Some(head) => FocusAction::Keep,
        Some(head) => FocusAction::Window(head.clone()),
        None if ctx.unfocus_on_empty_workspace => FocusAction::Clear,
        None => FocusAction::Keep,
    }
}

/// [`pointer_focus_ctx`] for a button press, touch-down or tablet tip: the
/// only input that may take the keyboard from an on-demand layer surface.
pub(crate) fn click_focus_ctx(state: &AbyssState, pos: Point<f64, Logical>) -> PointerFocusCtx {
    PointerFocusCtx {
        click: true,
        drag_active: crate::input::grabs::drag_active_but_click(state),
        ..pointer_focus_ctx(state, pos)
    }
}

/// Build a [`PointerFocusCtx`] from live state. Not pure, and deliberately the
/// only impure part: it reads, it does not decide.
pub(crate) fn pointer_focus_ctx(state: &AbyssState, pos: Point<f64, Logical>) -> PointerFocusCtx {
    let pointer_output =
        crate::shell::output_at(state, pos).and_then(|o| state.outputs.by_output(&o).map(|e| e.id));
    let focused = state.focus.clone();
    let focused_output = focused.as_ref().and_then(|w| output_of_window(state, w));
    let pointer_output_focus_head = pointer_output
        .and_then(|id| state.outputs.get(id))
        .and_then(|e| e.workspace().focus_head());
    let g = &state.config.general;
    let held = crate::shell::focused_layer(state);
    let layer_under = crate::shell::layer_at(state, pos).map(|l| LayerUnder {
        surface: l.wl_surface().clone(),
        interactivity: l.cached_state().keyboard_interactivity,
        holds_keyboard: held.as_ref() == Some(&l),
    });
    PointerFocusCtx {
        pointer_output,
        // Override-redirect menus/tooltips are not focus targets: the pointer
        // over one keeps the owning client's focus. A layer surface on top
        // hides whatever window is behind it.
        window_under: state
            .space
            .element_under(pos)
            .filter(|_| layer_under.is_none())
            .filter(|(w, _)| !w.x11_surface().is_some_and(|x| x.is_override_redirect()))
            .map(|(w, _)| w.clone()),
        layer_under,
        click: false,
        focused,
        focused_output,
        layer_interactivity: held.map(|l| l.cached_state().keyboard_interactivity),
        drag_active: crate::input::grabs::drag_active(state),
        prompt_grab_active: crate::trusted_ui::holds_seat(state),
        focus_follows_mouse_across_outputs: g.focus_follows_mouse_across_outputs,
        unfocus_on_empty_workspace: g.unfocus_on_empty_workspace,
        refocus_on_scene_change: g.refocus_on_scene_change,
        pointer_output_focus_head,
    }
}

/// Apply a verdict. The single place *pointer-driven* human-seat focus is
/// decided (ADR 0042); the move itself, and the focus-history write, are
/// `focus_window`'s, which keybinds, IPC and activation also go through.
pub fn apply_focus(state: &mut AbyssState, action: FocusAction, cause: FocusCause) {
    // The locker holds the keyboard; `unlock` re-derives focus afterwards.
    if state.lock.locked {
        return;
    }
    match action {
        FocusAction::Keep => {}
        FocusAction::Window(w) => {
            tracing::debug!(?cause, "focus moves to a window");
            focus_window_raising(state, &w, cause.raises());
        }
        FocusAction::Layer(surface) => {
            tracing::debug!(?cause, "focus moves to a layer surface");
            crate::shell::focus_layer_if_wanted(state, &surface);
        }
        FocusAction::Clear => {
            tracing::debug!(?cause, "focus cleared");
            state.focus = None;
            focus_surface(state, None);
            crate::ipc::emit(state, "focus", serde_json::json!({ "handle": null }));
        }
    }
}

pub fn focus_window(state: &mut AbyssState, window: &Window) {
    focus_window_raising(state, window, true);
}

/// [`focus_window`], with the RAISE-01 restack made optional. `raise = false`
/// records the focus history and moves keyboard focus but leaves the floating
/// stacking order alone; see [`FocusCause::raises`].
pub fn focus_window_raising(state: &mut AbyssState, window: &Window, raise: bool) {
    // Neither a trusted prompt (COMP-10) nor the locker may lose the keyboard
    // to a client, whatever asks (a click, IPC, activation, a window mapping);
    // `unlock` re-derives focus afterwards.
    if state.trusted_ui.active() || state.lock.locked {
        return;
    }
    // Override-redirect menus/tooltips never take focus or activation; the owning client keeps it.
    if window.x11_surface().is_some_and(|x| x.is_override_redirect()) {
        return;
    }
    let Some(surface) = window_surface(window) else {
        return;
    };
    // X11 focus is compositor-driven: activate, raise in the X stack, then
    // let the keyboard follow (COMP-07 §1).
    if let Some(x11) = window.x11_surface() {
        x11.set_activated(true).ok();
        if raise {
            if let Some(wm) = state.xwayland.wm.as_mut() {
                let _ = wm.raise_window(x11);
            }
        }
        let others: Vec<Window> = state.space.elements().filter(|w| *w != window).cloned().collect();
        for w in others {
            if let Some(other) = w.x11_surface() {
                other.set_activated(false).ok();
            }
        }
    }
    state.focus = Some(window.clone());
    state.urgent.retain(|w| w != window);
    if let Some(id) = output_of_window(state, window) {
        // The single write site for per-workspace focus history (ADR 0042).
        // Filed against the workspace that actually holds the window, not the
        // output's active one — focusing a window on an inactive workspace
        // would otherwise record it where it is not, and `focus_head` would
        // hand back a window from the wrong workspace.
        if let Some(entry) = state.outputs.get_mut(id) {
            if let Some(ws) = entry.workspaces.iter_mut().find(|ws| ws.holds(window)) {
                ws.note_focused(window, raise);
            }
        }
        if state.outputs.set_focused(id) {
            // Only on a real transition, mirroring input/mod.rs's pointer-motion
            // path: hyperion's fold logic keys off this "output" event, and
            // keyboard/alt-tab focus changes must drive it too, not just the
            // pointer crossing an output boundary.
            emit_output_state(state, id);
        }
    }
    // An X11 window is focused as its `X11Surface` so enter/leave set and
    // clear the X input focus; see `KeyboardFocusTarget`.
    let target = match window.x11_surface() {
        Some(x11) => KeyboardFocusTarget::X11(x11.clone()),
        None => KeyboardFocusTarget::Wl(surface),
    };
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, Some(target), SERIAL_COUNTER.next_serial());
    arrange(state);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(state, "focus", serde_json::json!({ "handle": handle }));
}

/// Restate the whole of what `hyperion` folds on, for output `id` (ADR 0042).
///
/// `focused` and `name` are the original payload. `fullscreen` and `idle` were
/// added because the bar cannot see either: it has no input access and no view
/// of the window stack, so the three inputs to its fold decision have to arrive
/// together or it folds on a stale one.
///
/// Emitted on every transition that changes one of them, not only on a focus
/// change — a workspace switch that lands on an empty workspace clears focus
/// without ever reaching [`focus_window`], and used to leave the bar folded.
pub fn emit_output_state(state: &mut AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let name = entry.connector.clone();
    let output = entry.output.clone();
    let fullscreen = crate::shell::output_has_fullscreen(state, &output);
    let idle = state.idle.bar_idle();
    crate::ipc::emit(
        state,
        "output",
        serde_json::json!({
            "focused": id,
            "name": name,
            "fullscreen": fullscreen,
            "idle": idle,
        }),
    );
}

/// Restate the focused output, if there is one. The idle and fullscreen paths
/// know something changed but not which output the bar cares about.
pub fn emit_focused_output_state(state: &mut AbyssState) {
    if let Some(id) = state.outputs.focused().map(|e| e.id) {
        emit_output_state(state, id);
    }
}

pub fn focus_surface(state: &mut AbyssState, surface: Option<WlSurface>) {
    // While locked only the locker's own surfaces may take the keyboard.
    if state.lock.locked && !surface.as_ref().is_some_and(|s| state.lock.owns(s)) {
        return;
    }
    // The destructive-action prompt keeps the keyboard until it is answered.
    if state.trusted_ui.active() {
        return;
    }
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, surface.map(Into::into), SERIAL_COUNTER.next_serial());
}

/// Re-derive focus for the focused output after the scene changed under it
/// (a window closed, a workspace switched, the lock lifted).
///
/// The focused output's own workspace answers this and nothing else: the
/// history head first, then its topmost window, then nothing. There is
/// deliberately no fall-back to the last window anywhere in the space — that
/// fall-back yanked focus across the desk when the last window on a monitor
/// closed (ADR 0042).
pub fn refocus_topmost(state: &mut AbyssState) {
    let (head, here) = match state.outputs.focused() {
        Some(entry) => (entry.workspace().focus_head(), entry.workspace().windows()),
        None => (None, Vec::new()),
    };
    let target = head.or_else(|| state.space.elements().rfind(|w| here.contains(w)).cloned());
    let action = match target {
        Some(w) => FocusAction::Window(w),
        None => FocusAction::Clear,
    };
    let cleared = matches!(action, FocusAction::Clear);
    apply_focus(state, action, FocusCause::WindowUnmap);
    if cleared {
        // `Clear` never reaches `focus_window`, so nothing above restated the
        // output. Switching to an empty workspace used to leave hyperion
        // folded with no event to unfold it.
        emit_focused_output_state(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Ctx = PointerFocusCtx<u32, u32>;

    /// The defaults every case starts from: pointer and focus on output 1, a
    /// focused window, no layer, no grab, every config key at its `true`
    /// default.
    fn ctx() -> Ctx {
        PointerFocusCtx {
            pointer_output: Some(1),
            window_under: None,
            layer_under: None,
            click: false,
            focused: Some(10),
            focused_output: Some(1),
            layer_interactivity: None,
            drag_active: false,
            prompt_grab_active: false,
            focus_follows_mouse_across_outputs: true,
            unfocus_on_empty_workspace: true,
            refocus_on_scene_change: true,
            pointer_output_focus_head: None,
        }
    }

    fn layer(surface: u32, interactivity: KeyboardInteractivity, holds_keyboard: bool) -> LayerUnder<u32> {
        LayerUnder {
            surface,
            interactivity,
            holds_keyboard,
        }
    }

    /// Every rule as a row: a name, the context, the verdict.
    fn table() -> Vec<(&'static str, Ctx, FocusAction<u32, u32>)> {
        vec![
            (
                "empty space on the focused output keeps focus",
                ctx(),
                FocusAction::Keep,
            ),
            (
                // A gap between monitors, or off the edge of every output.
                // There is nowhere to move focus to, so focus does not move.
                "the pointer over no output at all keeps focus",
                Ctx {
                    pointer_output: None,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                // The reported bug: pointer on an empty plane of monitor B
                // while a window on monitor A still ate the keystrokes.
                "empty space on another output takes that output's history head",
                Ctx {
                    pointer_output: Some(2),
                    pointer_output_focus_head: Some(20),
                    ..ctx()
                },
                FocusAction::Window(20),
            ),
            (
                "another output with an empty workspace clears focus",
                Ctx {
                    pointer_output: Some(2),
                    ..ctx()
                },
                FocusAction::Clear,
            ),
            (
                "unfocus-on-empty-workspace=false keeps focus instead of clearing",
                Ctx {
                    pointer_output: Some(2),
                    unfocus_on_empty_workspace: false,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "cross-output FFM off keeps focus on the other output",
                Ctx {
                    pointer_output: Some(2),
                    pointer_output_focus_head: Some(20),
                    focus_follows_mouse_across_outputs: false,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "a window under the pointer takes focus",
                Ctx {
                    window_under: Some(11),
                    ..ctx()
                },
                FocusAction::Window(11),
            ),
            (
                // An on-demand layer surface holds the keyboard after a click:
                // no toplevel is focused, so empty space must not clear it.
                "empty space with nothing focused keeps the keyboard where it is",
                Ctx {
                    pointer_output: Some(2),
                    focused: None,
                    focused_output: None,
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "the already-focused window under the pointer is a no-op",
                Ctx {
                    window_under: Some(10),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "an exclusive layer surface never loses the keyboard",
                Ctx {
                    window_under: Some(11),
                    layer_interactivity: Some(KeyboardInteractivity::Exclusive),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "an exclusive layer surface keeps the keyboard through a click on a window",
                Ctx {
                    window_under: Some(11),
                    layer_interactivity: Some(KeyboardInteractivity::Exclusive),
                    click: true,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "an exclusive layer surface keeps the keyboard through a click on another layer",
                Ctx {
                    layer_under: Some(layer(7, KeyboardInteractivity::OnDemand, false)),
                    layer_interactivity: Some(KeyboardInteractivity::Exclusive),
                    click: true,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                // The start menu being typed into; the pointer crosses a window.
                "hover over a window keeps an on-demand layer focused",
                Ctx {
                    window_under: Some(11),
                    focused: None,
                    focused_output: None,
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "hover onto another output keeps an on-demand layer focused",
                Ctx {
                    pointer_output: Some(2),
                    pointer_output_focus_head: Some(20),
                    window_under: Some(20),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "a click on a window moves the keyboard off an on-demand layer",
                Ctx {
                    window_under: Some(11),
                    focused: None,
                    focused_output: None,
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    click: true,
                    ..ctx()
                },
                FocusAction::Window(11),
            ),
            (
                // `focused` still naming the window is not "already focused":
                // the keyboard is on the layer.
                "a click on the last-focused window still takes the keyboard back",
                Ctx {
                    window_under: Some(10),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    click: true,
                    ..ctx()
                },
                FocusAction::Window(10),
            ),
            (
                "a click inside the on-demand layer itself keeps it",
                Ctx {
                    layer_under: Some(layer(7, KeyboardInteractivity::OnDemand, true)),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    click: true,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "a click on another focusable layer hands it the keyboard",
                Ctx {
                    layer_under: Some(layer(8, KeyboardInteractivity::OnDemand, false)),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    click: true,
                    ..ctx()
                },
                FocusAction::Layer(8),
            ),
            (
                "a click on a none layer returns the keyboard to the workspace head",
                Ctx {
                    layer_under: Some(layer(9, KeyboardInteractivity::None, false)),
                    focused: None,
                    focused_output: None,
                    pointer_output_focus_head: Some(10),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    click: true,
                    ..ctx()
                },
                FocusAction::Window(10),
            ),
            (
                "a click on an empty workspace clears the keyboard off an on-demand layer",
                Ctx {
                    focused: None,
                    focused_output: None,
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    click: true,
                    ..ctx()
                },
                FocusAction::Clear,
            ),
            (
                "hover over a panel never focuses the window behind it",
                Ctx {
                    window_under: Some(11),
                    layer_under: Some(layer(9, KeyboardInteractivity::None, false)),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "hover over an on-demand layer does not give it the keyboard",
                Ctx {
                    layer_under: Some(layer(8, KeyboardInteractivity::OnDemand, false)),
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                "a click on an on-demand layer gives it the keyboard",
                Ctx {
                    layer_under: Some(layer(8, KeyboardInteractivity::OnDemand, false)),
                    click: true,
                    ..ctx()
                },
                FocusAction::Layer(8),
            ),
            (
                "a click on a none layer leaves window focus alone",
                Ctx {
                    layer_under: Some(layer(9, KeyboardInteractivity::None, false)),
                    click: true,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
            (
                // A wallpaper is a layer too; it must not stop cross-output FFM.
                "a background layer on another output still takes that output's head",
                Ctx {
                    pointer_output: Some(2),
                    pointer_output_focus_head: Some(20),
                    layer_under: Some(layer(9, KeyboardInteractivity::None, false)),
                    ..ctx()
                },
                FocusAction::Window(20),
            ),
            (
                // COMP-05 §5: a drag must not cross an output boundary.
                "comp05_s5: a drag crossing outputs freezes focus at its origin",
                Ctx {
                    pointer_output: Some(2),
                    window_under: Some(20),
                    pointer_output_focus_head: Some(20),
                    drag_active: true,
                    ..ctx()
                },
                FocusAction::Keep,
            ),
        ]
    }

    #[test]
    fn pointer_focus_rules() {
        for (name, c, want) in table() {
            assert_eq!(decide_pointer_focus(&c), want, "{name}");
        }
    }

    /// COMP-10 §4: a trusted-UI prompt holding the seat outranks every other
    /// branch, so no configuration of any other field can move focus.
    #[test]
    fn a_prompt_grab_overrides_every_other_permutation() {
        for (name, mut c, _) in table() {
            c.prompt_grab_active = true;
            assert_eq!(
                decide_pointer_focus(&c),
                FocusAction::Keep,
                "prompt grab must outrank: {name}"
            );
        }
    }

    /// COMP-05 §5 (HARD): focus must not follow the mouse across an output
    /// boundary while a drag is in progress — dragging a file from monitor A
    /// to monitor B keeps focus on A until the button comes up. This is a
    /// deliberate deviation from Hyprland (ADR 0042); it is not a bug, and a
    /// failure here is a spec violation, not a regression in feel.
    #[test]
    fn comp05_s5_a_drag_never_crosses_an_output_boundary() {
        // Every shape the far output can be in: a window under the pointer, a
        // history head to steal, an empty workspace, cross-output FFM on or
        // off. None of them may move focus while a drag is in flight.
        let far = [
            Ctx {
                window_under: Some(20),
                ..ctx()
            },
            Ctx {
                pointer_output_focus_head: Some(20),
                ..ctx()
            },
            Ctx { ..ctx() },
            Ctx {
                focus_follows_mouse_across_outputs: false,
                ..ctx()
            },
            Ctx {
                layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                window_under: Some(20),
                ..ctx()
            },
        ];
        for (i, c) in far.into_iter().enumerate() {
            let dragging = Ctx {
                pointer_output: Some(2),
                drag_active: true,
                ..c.clone()
            };
            assert_eq!(
                decide_pointer_focus(&dragging),
                FocusAction::Keep,
                "case {i}: a drag crossing to another output must keep focus"
            );
            // The same context on the drag's *own* output is untouched by the
            // rule: freezing is about the boundary, not about drags at large.
            let same = Ctx {
                drag_active: true,
                ..c
            };
            assert_eq!(
                decide_pointer_focus(&same),
                decide_pointer_focus(&Ctx {
                    drag_active: false,
                    ..same.clone()
                }),
                "case {i}: a drag within one output must not change the verdict"
            );
        }
    }

    /// The other-output branch must not re-focus what is already focused: that
    /// would raise and re-emit on every pointer motion over empty space.
    #[test]
    fn the_history_head_already_focused_is_a_no_op() {
        let c = Ctx {
            pointer_output: Some(2),
            pointer_output_focus_head: Some(10),
            ..ctx()
        };
        assert_eq!(decide_pointer_focus(&c), FocusAction::Keep);
    }
}

/// State-backed tests for the impure half: [`pointer_focus_ctx`] reading real
/// geometry across two registered outputs, and [`apply_focus`] actually
/// mutating the seat. The pure table above covers the decision; this covers
/// the gather, which is where the cross-output bug actually lived.
///
/// A `Window` needs a real `ToplevelSurface` with a committed buffer before
/// `Space::element_under` will ever return it (smithay filters on `bbox()`),
/// so the tests that need windows drive a real in-process client
/// (`client` below); the rest are the conformance harness's job
/// (COMP-15 §1).
#[cfg(test)]
pub(crate) mod state_tests {
    use super::*;
    use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;
    use smithay::utils::Transform;
    use smithay::wayland::socket::ListeningSocketSource;

    const W: i32 = 800;
    const H: i32 = 600;

    /// Shared with `input`'s gesture tests, which need the same live state.
    pub(crate) struct Harness {
        pub(crate) state: AbyssState,
        pub(crate) a: u64,
        pub(crate) b: u64,
        // Dropped last; the state borrows nothing from them but the loop owns
        // the sources the state registered.
        event_loop: EventLoop<'static, AbyssState>,
        display: Display<AbyssState>,
    }

    fn output(name: &str) -> Output {
        Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "abyss".into(),
                model: "test".into(),
            },
        )
    }

    pub(crate) fn harness() -> Harness {
        // `ListeningSocketSource` needs somewhere to bind; a CI runner may have
        // no XDG_RUNTIME_DIR at all. `cargo test` runs tests on several threads,
        // so the set_var is done exactly once, before any harness proceeds.
        static RUNTIME_DIR: std::sync::Once = std::sync::Once::new();
        RUNTIME_DIR.call_once(|| {
            if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
                return;
            }
            let dir = std::env::temp_dir().join(format!("abyss-focus-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("runtime dir");
            // SAFETY: the only `set_var` in this binary, serialised by `Once`
            // and completed before any other test thread reads the variable.
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        });
        let event_loop: EventLoop<'static, AbyssState> = EventLoop::try_new().expect("calloop");
        let display: Display<AbyssState> = Display::new().expect("display");
        let socket = ListeningSocketSource::new_auto().expect("socket");
        let mut state = AbyssState::new(
            &display,
            event_loop.get_signal(),
            event_loop.handle(),
            &socket,
            crate::config::Config::default(),
            false,
        );
        let dh = state.display_handle.clone();
        let mode = Mode {
            size: (W, H).into(),
            refresh: 60_000,
        };
        let mut ids = Vec::new();
        for name in ["test-a", "test-b"] {
            let o = output(name);
            let global = o.create_global::<AbyssState>(&dh);
            o.change_current_state(Some(mode), Some(Transform::Normal), None, None);
            o.set_preferred(mode);
            ids.push(crate::outputs::register(
                &mut state,
                name.into(),
                name.into(),
                o,
                crate::outputs::OutputKind::Virtual,
                Some(global),
            ));
        }
        Harness {
            state,
            a: ids[0],
            b: ids[1],
            event_loop,
            display,
        }
    }

    /// A point inside `id`'s geometry, as the space actually laid it out — the
    /// arrangement is the compositor's to choose, so we ask rather than assume.
    fn inside(h: &Harness, id: u64) -> Point<f64, Logical> {
        let o = h.state.outputs.get(id).expect("output").output.clone();
        let g = h.state.space.output_geometry(&o).expect("mapped");
        (
            g.loc.x as f64 + g.size.w as f64 / 2.0,
            g.loc.y as f64 + g.size.h as f64 / 2.0,
        )
            .into()
    }

    #[test]
    fn the_pointer_output_tracks_which_output_the_cursor_is_over() {
        let h = harness();
        assert_ne!(h.a, h.b, "two distinct outputs");
        assert_eq!(
            pointer_focus_ctx(&h.state, inside(&h, h.a)).pointer_output,
            Some(h.a)
        );
        assert_eq!(
            pointer_focus_ctx(&h.state, inside(&h, h.b)).pointer_output,
            Some(h.b)
        );
    }

    /// The headline bug, minus the window: hovering another output's empty
    /// desktop must not leave focus parked where it was.
    #[test]
    fn empty_desktop_on_another_output_clears_rather_than_keeps() {
        let mut h = harness();
        h.state.outputs.set_focused(h.a);
        let pos = inside(&h, h.b);
        let ctx = pointer_focus_ctx(&h.state, pos);
        assert_eq!(ctx.pointer_output, Some(h.b));
        assert_eq!(ctx.window_under, None);
        assert_eq!(ctx.pointer_output_focus_head, None);
        // Nothing is focused here (the harness has no windows), so the verdict
        // is `Keep` — clearing nothing is not this rule's job. The pure table
        // covers the with-a-window case; what matters here is that the gather
        // saw the *other* output's empty workspace, not the focused one's.
        assert_eq!(decide_pointer_focus(&ctx), FocusAction::Keep);
        assert_eq!(
            decide_pointer_focus(&PointerFocusCtx::<u32, WlSurface> {
                pointer_output: ctx.pointer_output,
                window_under: None,
                layer_under: None,
                click: false,
                focused: Some(10),
                focused_output: Some(h.a),
                layer_interactivity: None,
                drag_active: ctx.drag_active,
                prompt_grab_active: ctx.prompt_grab_active,
                focus_follows_mouse_across_outputs: ctx.focus_follows_mouse_across_outputs,
                unfocus_on_empty_workspace: ctx.unfocus_on_empty_workspace,
                refocus_on_scene_change: ctx.refocus_on_scene_change,
                pointer_output_focus_head: None,
            }),
            FocusAction::Clear
        );
        apply_focus(&mut h.state, FocusAction::Clear, FocusCause::Pointer);
        assert!(h.state.focus.is_none());
    }

    /// Off every output the cursor still belongs to the focused one
    /// (`shell::output_at` falls back), so a pointer flung past the edge of the
    /// desktop never reads as "over nothing" and never disturbs the output the
    /// human is on. The `pointer_output.is_none()` arm in the decision is the
    /// belt to this braces.
    #[test]
    fn a_pointer_off_every_output_falls_back_to_the_focused_one() {
        let mut h = harness();
        h.state.outputs.set_focused(h.b);
        let ctx = pointer_focus_ctx(&h.state, (-10_000.0, -10_000.0).into());
        assert_eq!(ctx.pointer_output, Some(h.b));
    }

    #[test]
    fn the_cross_output_key_reaches_the_decision() {
        let mut h = harness();
        h.state.outputs.set_focused(h.a);
        h.state.config.general.focus_follows_mouse_across_outputs = false;
        let ctx = pointer_focus_ctx(&h.state, inside(&h, h.b));
        assert!(!ctx.focus_follows_mouse_across_outputs);
        assert_eq!(decide_pointer_focus(&ctx), FocusAction::Keep);
    }

    #[test]
    fn the_empty_workspace_key_reaches_the_decision() {
        let mut h = harness();
        h.state.outputs.set_focused(h.a);
        h.state.config.general.unfocus_on_empty_workspace = false;
        let ctx = pointer_focus_ctx(&h.state, inside(&h, h.b));
        assert!(!ctx.unfocus_on_empty_workspace);
        assert_eq!(decide_pointer_focus(&ctx), FocusAction::Keep);
    }
    /// The emit sites the bar's fold depends on (ADR 0042 amendment). Without
    /// these the bar's fold state goes stale: defect 4 was a workspace switch
    /// onto an empty workspace that told the bar nothing at all.
    fn outputs_emitted() -> Vec<serde_json::Value> {
        crate::ipc::capture::take()
            .into_iter()
            .filter(|(kind, _)| kind == "output")
            .map(|(_, data)| data)
            .collect()
    }

    #[test]
    fn switching_to_an_empty_workspace_still_restates_the_output() {
        let mut h = harness();
        h.state.outputs.set_focused(h.a);
        let _ = outputs_emitted();
        crate::shell::switch_workspace(&mut h.state, 3);
        let events = outputs_emitted();
        assert!(!events.is_empty(), "a workspace switch must emit `output`");
        let last = events.last().expect("event");
        assert_eq!(last["focused"].as_u64(), Some(h.a));
        // The three fold inputs all travel together.
        assert!(last.get("name").is_some());
        assert_eq!(last["fullscreen"].as_bool(), Some(false));
        assert_eq!(last["idle"].as_bool(), Some(false));
    }

    #[test]
    fn the_idle_latch_is_reported_to_the_bar() {
        let mut h = harness();
        h.state.outputs.set_focused(h.a);
        h.state.config.bar.fold_when_idle = true;
        h.state.config.bar.idle_seconds = 0;
        let _ = outputs_emitted();
        // One tick past the (zero) threshold flips the latch and says so.
        crate::input::idle::tick_for_test(&mut h.state);
        let events = outputs_emitted();
        assert_eq!(
            events.last().map(|e| e["idle"].as_bool()),
            Some(Some(true)),
            "the idle flip must reach the bar"
        );

        // And any input takes it straight back.
        crate::input::idle::on_activity(&mut h.state);
        let events = outputs_emitted();
        assert_eq!(events.last().map(|e| e["idle"].as_bool()), Some(Some(false)));
    }

    /// RAISE-01 raises on an explicit focus, never on a passive one. Passive
    /// raising restacked a window under a stationary pointer, so a window later
    /// moved beneath it never got the pointer (wlcs
    /// `surface_moves_over_surface_under_pointer`).
    #[test]
    fn only_explicit_focus_causes_raise() {
        for cause in [
            FocusCause::Click,
            FocusCause::Touch,
            FocusCause::Keybind,
            FocusCause::WindowMap,
            FocusCause::WorkspaceSwitch,
            FocusCause::Ipc,
        ] {
            assert!(cause.raises(), "{cause:?} should raise");
        }
        for cause in [FocusCause::Pointer, FocusCause::WindowUnmap] {
            assert!(!cause.raises(), "{cause:?} must not raise");
        }
    }

    /// A real in-process Wayland client, for the rules that only hold against
    /// real surfaces: the lock screen's.
    pub(crate) mod client {
        use std::os::fd::{AsFd, FromRawFd};
        use std::os::unix::net::UnixStream;

        use wayland_client::{
            delegate_noop,
            protocol::{
                wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_output::WlOutput, wl_registry,
                wl_seat::WlSeat, wl_shm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
            },
            Connection, Dispatch, EventQueue, Proxy, QueueHandle,
        };
        use wayland_protocols::ext::session_lock::v1::client::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
            ext_session_lock_v1::ExtSessionLockV1,
        };
        use wayland_protocols::xdg::shell::client::{
            xdg_surface::{self, XdgSurface},
            xdg_toplevel::{self, XdgToplevel},
            xdg_wm_base::{self, XdgWmBase},
        };

        use wayland_protocols_misc::zwp_input_method_v2::client::{
            zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
            zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
            zwp_input_method_v2::ZwpInputMethodV2,
        };

        use wayland_protocols_wlr::layer_shell::v1::client::{
            zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
            zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
        };

        use super::Harness;

        #[derive(Default)]
        pub struct Globals {
            list: Vec<(u32, String, u32)>,
            lock_size: Option<(u32, u32)>,
            /// `key` events an input-method keyboard grab received.
            pub grabbed_keys: usize,
            /// `zwlr_layer_surface_v1.configure` events acked so far.
            layer_configures: usize,
            /// Every `xdg_toplevel.configure` size, as (toplevel id, w, h).
            configures: Vec<(u32, i32, i32)>,
        }

        /// A toplevel's three client objects, for tests that drive its
        /// handshake one request at a time.
        pub struct Toplevel {
            pub surface: WlSurface,
            pub xdg: XdgSurface,
            pub toplevel: XdgToplevel,
        }

        pub struct Client {
            conn: Connection,
            queue: EventQueue<Globals>,
            data: Globals,
            compositor: Option<WlCompositor>,
            shm: Option<wl_shm::WlShm>,
            wm: Option<XdgWmBase>,
            outputs: Vec<WlOutput>,
            lock_manager: Option<ExtSessionLockManagerV1>,
            seat: Option<WlSeat>,
            im_manager: Option<ZwpInputMethodManagerV2>,
            layer_shell: Option<ZwlrLayerShellV1>,
        }

        impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
            fn event(
                g: &mut Self,
                _: &wl_registry::WlRegistry,
                event: wl_registry::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } = event
                {
                    g.list.push((name, interface, version));
                }
            }
        }

        impl Dispatch<XdgWmBase, ()> for Globals {
            fn event(
                _: &mut Self,
                wm: &XdgWmBase,
                event: xdg_wm_base::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let xdg_wm_base::Event::Ping { serial } = event {
                    wm.pong(serial);
                }
            }
        }

        impl Dispatch<XdgSurface, ()> for Globals {
            fn event(
                _: &mut Self,
                xdg: &XdgSurface,
                event: xdg_surface::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let xdg_surface::Event::Configure { serial } = event {
                    xdg.ack_configure(serial);
                }
            }
        }

        impl Dispatch<ExtSessionLockSurfaceV1, ()> for Globals {
            fn event(
                g: &mut Self,
                surface: &ExtSessionLockSurfaceV1,
                event: ext_session_lock_surface_v1::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let ext_session_lock_surface_v1::Event::Configure {
                    serial,
                    width,
                    height,
                } = event
                {
                    surface.ack_configure(serial);
                    g.lock_size = Some((width, height));
                }
            }
        }

        delegate_noop!(Globals: WlCompositor);
        delegate_noop!(Globals: WlShmPool);
        delegate_noop!(Globals: ExtSessionLockManagerV1);
        delegate_noop!(Globals: ignore wl_shm::WlShm);
        delegate_noop!(Globals: ignore WlBuffer);
        delegate_noop!(Globals: ignore WlSurface);
        delegate_noop!(Globals: ignore WlOutput);
        impl Dispatch<XdgToplevel, ()> for Globals {
            fn event(
                g: &mut Self,
                toplevel: &XdgToplevel,
                event: xdg_toplevel::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let xdg_toplevel::Event::Configure { width, height, .. } = event {
                    g.configures.push((toplevel.id().protocol_id(), width, height));
                }
            }
        }
        delegate_noop!(Globals: ignore ExtSessionLockV1);
        delegate_noop!(Globals: ignore WlSeat);
        delegate_noop!(Globals: ZwpInputMethodManagerV2);
        delegate_noop!(Globals: ignore ZwpInputMethodV2);
        delegate_noop!(Globals: ZwlrLayerShellV1);

        impl Dispatch<ZwlrLayerSurfaceV1, ()> for Globals {
            fn event(
                g: &mut Self,
                surface: &ZwlrLayerSurfaceV1,
                event: zwlr_layer_surface_v1::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
                    surface.ack_configure(serial);
                    g.layer_configures += 1;
                }
            }
        }

        impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for Globals {
            fn event(
                g: &mut Self,
                _: &ZwpInputMethodKeyboardGrabV2,
                event: zwp_input_method_keyboard_grab_v2::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let zwp_input_method_keyboard_grab_v2::Event::Key { .. } = event {
                    g.grabbed_keys += 1;
                }
            }
        }

        impl Client {
            pub fn connect(h: &mut Harness) -> Self {
                let (server, client) = UnixStream::pair().expect("socket pair");
                h.state
                    .display_handle
                    .insert_client(server, crate::state::client_state())
                    .expect("insert client");
                let conn = Connection::from_socket(client).expect("connect");
                let queue = conn.new_event_queue();
                let registry = conn.display().get_registry(&queue.handle(), ());
                let mut c = Self {
                    conn,
                    queue,
                    data: Globals::default(),
                    compositor: None,
                    shm: None,
                    wm: None,
                    outputs: Vec::new(),
                    lock_manager: None,
                    seat: None,
                    im_manager: None,
                    layer_shell: None,
                };
                c.pump(h);
                let qh = c.queue.handle();
                for (name, interface, version) in std::mem::take(&mut c.data.list) {
                    match interface.as_str() {
                        "wl_compositor" => c.compositor = Some(registry.bind(name, version.min(5), &qh, ())),
                        "wl_shm" => c.shm = Some(registry.bind(name, 1, &qh, ())),
                        "xdg_wm_base" => c.wm = Some(registry.bind(name, 1, &qh, ())),
                        "wl_output" => c.outputs.push(registry.bind(name, 1, &qh, ())),
                        "wl_seat" => c.seat = Some(registry.bind(name, 1, &qh, ())),
                        "zwp_input_method_manager_v2" => c.im_manager = Some(registry.bind(name, 1, &qh, ())),
                        "zwlr_layer_shell_v1" => {
                            c.layer_shell = Some(registry.bind(name, version.min(4), &qh, ()))
                        }
                        "ext_session_lock_manager_v1" => {
                            c.lock_manager = Some(registry.bind(name, 1, &qh, ()))
                        }
                        _ => {}
                    }
                }
                c.pump(h);
                c
            }

            /// Run both ends until the traffic settles.
            pub fn pump(&mut self, h: &mut Harness) {
                for _ in 0..8 {
                    self.conn.flush().expect("client flush");
                    h.display.dispatch_clients(&mut h.state).expect("server dispatch");
                    h.event_loop
                        .dispatch(std::time::Duration::ZERO, &mut h.state)
                        .expect("loop dispatch");
                    h.display.flush_clients().expect("server flush");
                    if let Some(guard) = self.conn.prepare_read() {
                        let _ = guard.read();
                    }
                    self.queue
                        .dispatch_pending(&mut self.data)
                        .expect("client dispatch");
                }
            }

            fn buffer(&self, w: i32, h: i32) -> WlBuffer {
                let size = w * h * 4;
                // SAFETY: an anonymous memfd; the name is a valid C string.
                let fd = unsafe { libc::memfd_create(c"abyss-test".as_ptr(), 0) };
                assert!(fd >= 0, "memfd_create");
                // SAFETY: `fd` was just created and nothing else owns it.
                let file = unsafe { std::fs::File::from_raw_fd(fd) };
                file.set_len(size as u64).expect("size memfd");
                let qh = self.queue.handle();
                let pool = self
                    .shm
                    .as_ref()
                    .expect("wl_shm")
                    .create_pool(file.as_fd(), size, &qh, ());
                let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
                pool.destroy();
                buffer
            }

            fn surface(&self) -> WlSurface {
                self.compositor
                    .as_ref()
                    .expect("wl_compositor")
                    .create_surface(&self.queue.handle(), ())
            }

            /// Map a 100x100 xdg toplevel; returns its surface's protocol id.
            pub fn map_window(&mut self, h: &mut Harness) -> u32 {
                let qh = self.queue.handle();
                let surface = self.surface();
                let xdg = self
                    .wm
                    .as_ref()
                    .expect("xdg_wm_base")
                    .get_xdg_surface(&surface, &qh, ());
                let _toplevel = xdg.get_toplevel(&qh, ());
                surface.commit();
                self.pump(h);
                surface.attach(Some(&self.buffer(100, 100)), 0, 0);
                surface.commit();
                self.pump(h);
                surface.id().protocol_id()
            }

            /// `get_toplevel` and nothing else, delivered: the compositor has
            /// seen the role but not the initial commit.
            pub fn create_toplevel(&mut self, h: &mut Harness) -> Toplevel {
                let qh = self.queue.handle();
                let surface = self.surface();
                let xdg = self
                    .wm
                    .as_ref()
                    .expect("xdg_wm_base")
                    .get_xdg_surface(&surface, &qh, ());
                let toplevel = xdg.get_toplevel(&qh, ());
                self.pump(h);
                Toplevel {
                    surface,
                    xdg,
                    toplevel,
                }
            }

            /// Commit `surface` as it stands and deliver it.
            pub fn commit(&mut self, h: &mut Harness, surface: &WlSurface) {
                surface.commit();
                self.pump(h);
            }

            /// Attach a 100x100 buffer, commit and deliver: the map.
            pub fn attach(&mut self, h: &mut Harness, surface: &WlSurface) {
                surface.attach(Some(&self.buffer(100, 100)), 0, 0);
                self.commit(h, surface);
            }

            /// Every size `toplevel` has been configured with, oldest first.
            pub fn configured_sizes(&self, toplevel: &XdgToplevel) -> Vec<(i32, i32)> {
                let id = toplevel.id().protocol_id();
                self.data
                    .configures
                    .iter()
                    .filter(|(t, _, _)| *t == id)
                    .map(|&(_, w, h)| (w, h))
                    .collect()
            }

            /// An input method on the seat, with the keyboard grabbed.
            pub fn grab_keyboard(&mut self, h: &mut Harness) -> ZwpInputMethodV2 {
                let qh = self.queue.handle();
                let im = self
                    .im_manager
                    .as_ref()
                    .expect("zwp_input_method_manager_v2")
                    .get_input_method(self.seat.as_ref().expect("wl_seat"), &qh, ());
                self.regrab(h, &im);
                im
            }

            pub fn regrab(&mut self, h: &mut Harness, im: &ZwpInputMethodV2) {
                let _grab = im.grab_keyboard(&self.queue.handle(), ());
                self.pump(h);
            }

            /// Map a 50x50 top-layer surface in the bottom-right corner of the
            /// first output, with the given keyboard interactivity.
            pub fn map_layer(
                &mut self,
                h: &mut Harness,
                interactivity: zwlr_layer_surface_v1::KeyboardInteractivity,
            ) -> (WlSurface, ZwlrLayerSurfaceV1) {
                let qh = self.queue.handle();
                let surface = self.surface();
                let layer = self
                    .layer_shell
                    .as_ref()
                    .expect("zwlr_layer_shell_v1")
                    .get_layer_surface(
                        &surface,
                        self.outputs.first(),
                        zwlr_layer_shell_v1::Layer::Top,
                        "test".into(),
                        &qh,
                        (),
                    );
                layer.set_size(50, 50);
                layer
                    .set_anchor(zwlr_layer_surface_v1::Anchor::Bottom | zwlr_layer_surface_v1::Anchor::Right);
                layer.set_keyboard_interactivity(interactivity);
                let before = self.data.layer_configures;
                self.commit(h, &surface);
                assert!(self.data.layer_configures > before, "layer surface configured");
                surface.attach(Some(&self.buffer(50, 50)), 0, 0);
                self.commit(h, &surface);
                (surface, layer)
            }

            pub fn grabbed_keys(&self) -> usize {
                self.data.grabbed_keys
            }

            pub fn lock(&mut self, h: &mut Harness) -> ExtSessionLockV1 {
                let lock = self
                    .lock_manager
                    .as_ref()
                    .expect("ext_session_lock_manager_v1")
                    .lock(&self.queue.handle(), ());
                self.pump(h);
                lock
            }

            /// A lock surface on every output, each with a committed buffer.
            pub fn lock_surfaces(&mut self, h: &mut Harness, lock: &ExtSessionLockV1) {
                let qh = self.queue.handle();
                for output in self.outputs.clone() {
                    let surface = self.surface();
                    let _lock_surface = lock.get_lock_surface(&surface, &output, &qh, ());
                    self.data.lock_size = None;
                    self.pump(h);
                    let (w, ht) = self.data.lock_size.expect("lock surface configured");
                    surface.attach(Some(&self.buffer(w as i32, ht as i32)), 0, 0);
                    surface.commit();
                    self.pump(h);
                }
            }
        }
    }

    fn window_by_protocol_id(h: &Harness, id: u32) -> Window {
        h.state
            .space
            .elements()
            .find(|w| {
                window_surface(w)
                    .is_some_and(|s| smithay::reexports::wayland_server::Resource::id(&s).protocol_id() == id)
            })
            .cloned()
            .expect("mapped window")
    }

    fn keyboard_on_lock_surface(h: &Harness) -> bool {
        match h.state.seat.get_keyboard().unwrap().current_focus() {
            Some(crate::protocols::standard::seat::KeyboardFocusTarget::Wl(s)) => h.state.lock.owns(&s),
            _ => false,
        }
    }

    /// Engaging the lock drops pointer focus from the surface behind it;
    /// otherwise the next button goes to that app before any motion clears it.
    #[test]
    fn locking_clears_pointer_focus() {
        let mut h = harness();
        let mut c = client::Client::connect(&mut h);
        let id = c.map_window(&mut h);
        let window = window_by_protocol_id(&h, id);
        let surface = window_surface(&window).expect("surface");
        let pointer = h.state.seat.get_pointer().unwrap();
        let loc = h.state.space.element_location(&window).expect("placed").to_f64();
        pointer.motion(
            &mut h.state,
            Some((surface.clone(), loc)),
            &smithay::input::pointer::MotionEvent {
                location: loc,
                serial: SERIAL_COUNTER.next_serial(),
                time: 0,
            },
        );
        assert_eq!(pointer.current_focus(), Some(surface));

        let _lock = c.lock(&mut h);
        assert!(h.state.lock.locked);
        assert_eq!(pointer.current_focus(), None);
    }

    /// An input-method keyboard grab gets every key whatever holds focus, so
    /// under the lock it would receive the unlock password.
    #[test]
    fn no_keyboard_grab_survives_the_lock() {
        use smithay::backend::input::KeyState;

        let mut h = harness();
        let mut c = client::Client::connect(&mut h);
        let im = c.grab_keyboard(&mut h);
        let keyboard = h.state.seat.get_keyboard().unwrap();
        assert!(keyboard.is_grabbed(), "the input method holds a grab");

        let lock = c.lock(&mut h);
        c.lock_surfaces(&mut h, &lock);
        assert!(!keyboard.is_grabbed(), "locking ends the grab");

        // Grabbing again while locked lands, but no key reaches it.
        c.regrab(&mut h, &im);
        assert!(keyboard.is_grabbed());
        let before = c.grabbed_keys();
        // evdev KEY_A, as an xkb keycode.
        h.state.keyboard_key(38u32.into(), KeyState::Pressed, 1);
        h.state.keyboard_key(38u32.into(), KeyState::Released, 2);
        c.pump(&mut h);
        assert!(!keyboard.is_grabbed());
        assert_eq!(c.grabbed_keys(), before, "no key reaches the input method");
        assert!(keyboard_on_lock_surface(&h));
    }

    /// The reported hole: a click over a window behind the lock moved keyboard
    /// focus to it, and the unlock password went to that app.
    #[test]
    fn a_click_under_the_lock_never_moves_keyboard_focus() {
        use smithay::backend::input::ButtonState;

        let mut h = harness();
        let mut c = client::Client::connect(&mut h);
        let first = c.map_window(&mut h);
        let second = c.map_window(&mut h);
        let windows = [
            window_by_protocol_id(&h, first),
            window_by_protocol_id(&h, second),
        ];
        let focused = h.state.focus.clone().expect("a mapped window is focused");
        let other = windows
            .iter()
            .find(|w| **w != focused)
            .expect("an unfocused window")
            .clone();
        let loc = h.state.space.element_location(&other).expect("placed");
        let over_other = (loc + Point::from((10, 10))).to_f64();

        let lock = c.lock(&mut h);
        c.lock_surfaces(&mut h, &lock);
        assert!(keyboard_on_lock_surface(&h), "the locker holds the keyboard");

        // Ungated, this click takes focus: the rules alone say so.
        assert_eq!(
            decide_pointer_focus(&pointer_focus_ctx(&h.state, over_other)),
            FocusAction::Window(other.clone())
        );

        h.state.pointer_moved(over_other, 1);
        let pointer = h.state.seat.get_pointer().unwrap();
        // The locker is keyboard-only (ADR 0024): no surface is under the pointer.
        assert_eq!(
            pointer.current_focus(),
            None,
            "motion under the lock reaches no surface"
        );
        h.state.pointer_button(0x110, ButtonState::Pressed, 2);
        h.state.pointer_button(0x110, ButtonState::Released, 3);
        assert!(keyboard_on_lock_surface(&h), "a click must not take the keyboard");
        assert_eq!(
            pointer.current_focus(),
            None,
            "a click under the lock reaches no surface"
        );

        // Defense in depth: the focus primitives refuse too.
        focus_window(&mut h.state, &other);
        assert!(keyboard_on_lock_surface(&h));
        apply_focus(&mut h.state, FocusAction::Clear, FocusCause::Click);
        assert!(keyboard_on_lock_surface(&h));
        focus_surface(&mut h.state, window_surface(&other));
        assert!(keyboard_on_lock_surface(&h));
    }

    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity as ClientKi;

    fn keyboard_on_window(h: &Harness, window: &Window) -> bool {
        match h.state.seat.get_keyboard().unwrap().current_focus() {
            Some(KeyboardFocusTarget::Wl(s)) => window_surface(window).as_ref() == Some(&s),
            _ => false,
        }
    }

    fn keyboard_on_layer(h: &Harness) -> bool {
        crate::shell::focused_layer(&h.state).is_some()
    }

    /// A mapped window and a point over it that no layer surface covers.
    fn window_and_point(h: &mut Harness, c: &mut client::Client) -> (Window, Point<f64, Logical>) {
        let id = c.map_window(h);
        let window = window_by_protocol_id(h, id);
        let loc = h.state.space.element_location(&window).expect("placed");
        let over = (loc + Point::from((10, 10))).to_f64();
        (window, over)
    }

    /// A point over the test layer surface: 10px in from the bottom-right
    /// corner of whichever output it landed on.
    fn over_layer(h: &Harness) -> Point<f64, Logical> {
        for e in h.state.outputs.iter() {
            let g = h.state.space.output_geometry(&e.output).expect("mapped");
            let p: Point<f64, Logical> =
                ((g.loc.x + g.size.w - 10) as f64, (g.loc.y + g.size.h - 10) as f64).into();
            if crate::shell::layer_at(&h.state, p).is_some() {
                return p;
            }
        }
        panic!("the layer surface is under no output corner");
    }

    fn press(h: &mut Harness, c: &mut client::Client, t: u32) {
        use smithay::backend::input::ButtonState;
        h.state.pointer_button(0x110, ButtonState::Pressed, t);
        h.state.pointer_button(0x110, ButtonState::Released, t + 1);
        c.pump(h);
    }

    /// The start menu: an on-demand layer surface being typed into keeps the
    /// keyboard while the pointer crosses a window, loses it to a click on the
    /// window, and a click on the layer gives it back (wlr-layer-shell
    /// `on_demand`, "requiring a click even if focus follows the mouse").
    #[test]
    fn an_on_demand_layer_is_released_by_a_click_not_by_hover() {
        let mut h = harness();
        let mut c = client::Client::connect(&mut h);
        let (window, over_window) = window_and_point(&mut h, &mut c);
        let (_surface, _layer) = c.map_layer(&mut h, ClientKi::OnDemand);
        assert!(keyboard_on_layer(&h), "mapping on-demand takes the keyboard");
        assert!(crate::shell::layer_at(&h.state, over_window).is_none());
        assert!(h.state.config.general.focus_follows_mouse);

        h.state.pointer_moved(over_window, 1);
        c.pump(&mut h);
        assert!(
            keyboard_on_layer(&h),
            "hover over a window keeps the layer focused"
        );

        press(&mut h, &mut c, 2);
        assert!(
            !keyboard_on_layer(&h),
            "a click on a window takes the keyboard off the layer"
        );
        assert!(keyboard_on_window(&h, &window));
        assert_eq!(h.state.focus.as_ref(), Some(&window));

        // Hovering the layer does not give it the keyboard; clicking it does.
        let on_layer = over_layer(&h);
        h.state.pointer_moved(on_layer, 4);
        c.pump(&mut h);
        assert!(
            keyboard_on_window(&h, &window),
            "hover over a layer never takes focus"
        );
        press(&mut h, &mut c, 5);
        assert!(
            keyboard_on_layer(&h),
            "a click on an on-demand layer takes the keyboard"
        );
    }

    /// wlr-layer-shell `exclusive` is unchanged: neither hover nor a click on
    /// a window takes the keyboard from it.
    #[test]
    fn an_exclusive_layer_keeps_the_keyboard_through_hover_and_click() {
        let mut h = harness();
        let mut c = client::Client::connect(&mut h);
        let (_window, over_window) = window_and_point(&mut h, &mut c);
        let (_surface, _layer) = c.map_layer(&mut h, ClientKi::Exclusive);
        assert!(keyboard_on_layer(&h));

        h.state.pointer_moved(over_window, 1);
        c.pump(&mut h);
        assert!(keyboard_on_layer(&h), "hover keeps an exclusive layer focused");
        press(&mut h, &mut c, 2);
        assert!(keyboard_on_layer(&h), "a click keeps an exclusive layer focused");
    }

    /// A mapped layer surface that changes its interactivity: asking for the
    /// keyboard takes it, giving it up hands it back to the workspace.
    #[test]
    fn an_interactivity_change_moves_the_keyboard() {
        let mut h = harness();
        let mut c = client::Client::connect(&mut h);
        let (window, _) = window_and_point(&mut h, &mut c);
        let (surface, layer) = c.map_layer(&mut h, ClientKi::None);
        assert!(!keyboard_on_layer(&h), "a none layer never takes the keyboard");
        assert!(keyboard_on_window(&h, &window));

        layer.set_keyboard_interactivity(ClientKi::OnDemand);
        c.commit(&mut h, &surface);
        assert!(keyboard_on_layer(&h), "none -> on_demand takes the keyboard");

        layer.set_keyboard_interactivity(ClientKi::None);
        c.commit(&mut h, &surface);
        assert!(!keyboard_on_layer(&h), "-> none gives it up");
        assert!(keyboard_on_window(&h, &window), "back to the workspace's head");

        layer.set_keyboard_interactivity(ClientKi::Exclusive);
        c.commit(&mut h, &surface);
        assert!(keyboard_on_layer(&h), "none -> exclusive takes the keyboard");
    }
}
