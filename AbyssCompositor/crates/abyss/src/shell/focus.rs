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
    shell::{arrange, output_of_window, window_surface},
    state::AbyssState,
};

/// What should happen to keyboard focus. Generic only so the decision rules can
/// be unit-tested without standing up a real `Window` (which needs a live
/// toplevel surface), exactly as `workspace::FocusHistory<T>` is; everything
/// outside the tests uses the `Window` default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusAction<W = Window> {
    /// Leave focus exactly where it is. Not "refocus what is focused" — a
    /// genuine no-op, so it cannot raise, re-emit or re-order history.
    Keep,
    Window(W),
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

/// Everything [`decide_pointer_focus`] is allowed to look at. Deliberately
/// plain data: no `&AbyssState`, no smithay handles beyond the window type.
#[derive(Debug, Clone)]
pub(crate) struct PointerFocusCtx<W = Window> {
    /// Output under the pointer. `None` when the pointer is over no output.
    pub pointer_output: Option<u64>,
    /// Topmost toplevel under the pointer, if any.
    pub window_under: Option<W>,
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
    /// `general.focus-follows-mouse-layers`
    pub focus_follows_mouse_layers: bool,
    /// `general.refocus-on-scene-change`. Not consulted by the pointer rules;
    /// it gates the *scene-change* refocus path in
    /// `AbyssState::refresh_pointer_focus`, which reads it off this context.
    pub refocus_on_scene_change: bool,
    /// Focus-history head of the workspace on `pointer_output`.
    pub pointer_output_focus_head: Option<W>,
}

/// Pure. The whole pointer-focus rule set, in precedence order.
pub(crate) fn decide_pointer_focus<W: Clone + PartialEq>(ctx: &PointerFocusCtx<W>) -> FocusAction<W> {
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
    //    Escape and typing do nothing. `OnDemand` may lose it, if configured.
    match ctx.layer_interactivity {
        Some(KeyboardInteractivity::Exclusive) => return FocusAction::Keep,
        Some(KeyboardInteractivity::OnDemand) if !ctx.focus_follows_mouse_layers => return FocusAction::Keep,
        _ => {}
    }

    // 4. A window under the pointer takes it, unless it already has it.
    if let Some(w) = ctx.window_under.as_ref() {
        if ctx.focused.as_ref() == Some(w) {
            return FocusAction::Keep;
        }
        return FocusAction::Window(w.clone());
    }

    // 5. Nothing has the keyboard, or something that is not a toplevel does —
    //    an on-demand layer surface the human clicked, say. There is no window
    //    focus to move and none to clear, so the empty-space rules below have
    //    nothing to say: clearing here would take the keyboard away from a
    //    layer surface that just earned it (wlcs
    //    LayerSurfaceTest.takes_keyboard_focus_after_click_with_on_demand_*).
    if ctx.focused.is_none() {
        return FocusAction::Keep;
    }

    // 6. The pointer is over no output at all — a gap between monitors, or
    //    past the edge of every one. There is no output to move to, so there
    //    is nothing to decide: dragging the cursor through dead space must
    //    never defocus (it is the exact inverse of the stickiness rule below).
    if ctx.pointer_output.is_none() {
        return FocusAction::Keep;
    }

    // 7. Empty space. On the focused window's own output this changes nothing
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
    PointerFocusCtx {
        pointer_output,
        window_under: state.space.element_under(pos).map(|(w, _)| w.clone()),
        focused,
        focused_output,
        layer_interactivity: crate::shell::focused_layer(state)
            .map(|l| l.cached_state().keyboard_interactivity),
        drag_active: crate::input::grabs::drag_active(state),
        // TODO(step 6: trusted UI): `trusted_ui/` does not exist yet, so no
        // prompt can hold the seat and `false` is correct today. This becomes
        // a read of the prompt grab when COMP-10 lands.
        prompt_grab_active: false,
        focus_follows_mouse_across_outputs: g.focus_follows_mouse_across_outputs,
        unfocus_on_empty_workspace: g.unfocus_on_empty_workspace,
        focus_follows_mouse_layers: g.focus_follows_mouse_layers,
        refocus_on_scene_change: g.refocus_on_scene_change,
        pointer_output_focus_head,
    }
}

/// Apply a verdict. The single place *pointer-driven* human-seat focus is
/// decided (ADR 0042); the move itself, and the focus-history write, are
/// `focus_window`'s, which keybinds, IPC and activation also go through.
pub fn apply_focus(state: &mut AbyssState, action: FocusAction, cause: FocusCause) {
    match action {
        FocusAction::Keep => {}
        FocusAction::Window(w) => {
            tracing::debug!(?cause, "focus moves to a window");
            focus_window_raising(state, &w, cause.raises());
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
            // path: eclipse-bar's fold logic keys off this "output" event, and
            // keyboard/alt-tab focus changes must drive it too, not just the
            // pointer crossing an output boundary.
            emit_output_state(state, id);
        }
    }
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
    arrange(state);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(state, "focus", serde_json::json!({ "handle": handle }));
}

/// Restate the whole of what `eclipse-bar` folds on, for output `id` (ADR 0042).
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
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, surface, SERIAL_COUNTER.next_serial());
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
        // output. Switching to an empty workspace used to leave eclipse-bar
        // folded with no event to unfold it.
        emit_focused_output_state(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Ctx = PointerFocusCtx<u32>;

    /// The defaults every case starts from: pointer and focus on output 1, a
    /// focused window, no layer, no grab, every config key at its `true`
    /// default.
    fn ctx() -> Ctx {
        PointerFocusCtx {
            pointer_output: Some(1),
            window_under: None,
            focused: Some(10),
            focused_output: Some(1),
            layer_interactivity: None,
            drag_active: false,
            prompt_grab_active: false,
            focus_follows_mouse_across_outputs: true,
            unfocus_on_empty_workspace: true,
            focus_follows_mouse_layers: true,
            refocus_on_scene_change: true,
            pointer_output_focus_head: None,
        }
    }

    /// Every rule as a row: a name, the context, the verdict.
    fn table() -> Vec<(&'static str, Ctx, FocusAction<u32>)> {
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
                "an on-demand layer surface may lose the keyboard to FFM",
                Ctx {
                    window_under: Some(11),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    ..ctx()
                },
                FocusAction::Window(11),
            ),
            (
                "focus-follows-mouse-layers=false pins an on-demand layer",
                Ctx {
                    window_under: Some(11),
                    layer_interactivity: Some(KeyboardInteractivity::OnDemand),
                    focus_follows_mouse_layers: false,
                    ..ctx()
                },
                FocusAction::Keep,
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
/// Windows are deliberately absent: a `Window` needs a real `ToplevelSurface`
/// with a committed buffer before `Space::element_under` will ever return it
/// (smithay filters on `bbox()`), which needs a real client. Those cases are
/// the conformance harness's job (COMP-15 §1), not this module's.
#[cfg(test)]
mod state_tests {
    use super::*;
    use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;
    use smithay::utils::Transform;
    use smithay::wayland::socket::ListeningSocketSource;

    const W: i32 = 800;
    const H: i32 = 600;

    struct Harness {
        state: AbyssState,
        a: u64,
        b: u64,
        // Dropped last; the state borrows nothing from them but the loop owns
        // the sources the state registered.
        _loop: EventLoop<'static, AbyssState>,
        _display: Display<AbyssState>,
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

    fn harness() -> Harness {
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
            _loop: event_loop,
            _display: display,
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
            decide_pointer_focus(&PointerFocusCtx::<u32> {
                pointer_output: ctx.pointer_output,
                window_under: None,
                focused: Some(10),
                focused_output: Some(h.a),
                layer_interactivity: None,
                drag_active: ctx.drag_active,
                prompt_grab_active: ctx.prompt_grab_active,
                focus_follows_mouse_across_outputs: ctx.focus_follows_mouse_across_outputs,
                unfocus_on_empty_workspace: ctx.unfocus_on_empty_workspace,
                focus_follows_mouse_layers: ctx.focus_follows_mouse_layers,
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
}
