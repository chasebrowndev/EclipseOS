// SPDX-License-Identifier: AGPL-3.0-only
//! Control-socket method handlers (COMP-13 §2.1).
//!
//! Every function here runs only after [`super::gate::check`] returned
//! `Allow`, on the compositor thread, between two frames. They may touch
//! `AbyssState` freely; they must not block.
//!
//! What is deliberately absent is as much of the design as what is present:
//! no `get_tree`, no capture, no grant manipulation, and no window *content*
//! of any kind (COMP-13 §2).

use serde_json::{json, Map, Value};
use smithay::reexports::wayland_server::Resource;
use smithay::{
    desktop::Window,
    utils::{IsAlive, Logical, Rectangle},
    wayland::compositor::with_states,
};

use super::RpcError;
use crate::{
    contain::{Feature, Query},
    state::AbyssState,
};

type Reply = Result<Value, RpcError>;

/// Run one method of a contained feature (ADR 0102). Off, whether just now or
/// earlier in the session, the method is refused before it runs.
fn contained(state: &mut AbyssState, feature: Feature, f: impl FnOnce(&mut AbyssState) -> Reply) -> Reply {
    crate::contain::contain(state, feature, f).unwrap_or_else(|| {
        Err(RpcError::denied(&format!(
            "{} is off for this session after an internal error",
            feature.name()
        )))
    })
}

/// [`contained`] for one query method: only that method turns off.
fn query(state: &mut AbyssState, q: Query, f: impl FnOnce(&mut AbyssState) -> Reply) -> Reply {
    crate::contain::contain_query(state, q, f).unwrap_or_else(|| {
        Err(RpcError::denied(&format!(
            "{} is off for this session after an internal error",
            q.name()
        )))
    })
}

pub fn dispatch(state: &mut AbyssState, conn: u64, method: &str, params: &Value) -> Reply {
    match method {
        // Queries. Read-only, so a panic in one is contained (ADR 0102): the
        // reply being built unwinds unsent and the queries turn off.
        "get_workspaces" => query(state, Query::GetWorkspaces, get_workspaces),
        "get_desktop_grid" => query(state, Query::GetDesktopGrid, |s| get_desktop_grid(s, params)),
        "get_outputs" => query(state, Query::GetOutputs, |s| get_outputs(s, params)),
        "get_windows" => query(state, Query::GetWindows, get_windows),
        "get_focused" => query(state, Query::GetFocused, get_focused),
        "get_metrics" => query(state, Query::GetMetrics, get_metrics),
        "dump_state" => query(state, Query::DumpState, dump_state),
        // Subscription.
        "subscribe" => subscribe(state, conn, params),
        "unsubscribe" => unsubscribe(state, conn),
        // Commands.
        "focus_window" => focus_window(state, params),
        "close_window" => close_window(state, params),
        "move_to_workspace" => move_to_workspace(state, params),
        "set_floating" => set_floating(state, params),
        "set_minimized" => set_minimized(state, params),
        "set_window_chip_rect" => set_window_chip_rect(state, params),
        "set_client_strips" => super::client_strips::set_client_strips(state, conn, params),
        "switch_workspace" => switch_workspace(state, params),
        "switch_desktop_direction" => switch_desktop_direction(state, params),
        "open_window_menu" => open_window_menu(state, params),
        "show_window_preview" => contained(state, Feature::Overview, |s| show_window_preview(s, params)),
        "hide_window_preview" => contained(state, Feature::Overview, |s| hide_window_preview(s, params)),
        "resize" => resize(state, params),
        "move_workspace_to_output" => move_workspace_to_output(state, params),
        "set_output" => set_output(state, params),
        "calibrate_output" => calibrate_output(state, params),
        "reload_config" => reload_config(state),
        // Scripted input (COMP-13 §2.2); the gate already required the
        // `misc { scripted-input }` toggle to reach these. Tagged `Scripted`,
        // so a protected surface never takes it.
        "click_at" => scripted(state, |s| click_at(s, params)),
        "type_text" => scripted(state, |s| type_text(s, params)),
        "press_key" => scripted(state, |s| press_key(s, params)),
        "scroll_at" => scripted(state, |s| scroll_at(s, params)),
        "drag_at" => scripted(state, |s| drag_at(s, params)),
        // Annotation overlays (COMP-18 §3). `conn` is the owner: a caller may
        // address only the handles it created.
        // Contained (ADR 0102): a panic drops every annotation and turns them off.
        "annotation_create" => contained(state, Feature::Annotations, |s| {
            annotation_create(s, conn, params)
        }),
        "annotation_update" => contained(state, Feature::Annotations, |s| {
            annotation_update(s, conn, params)
        }),
        "annotation_destroy" => contained(state, Feature::Annotations, |s| {
            annotation_destroy(s, conn, params)
        }),
        "annotation_clear" => contained(state, Feature::Annotations, |s| annotation_clear(s, conn, params)),
        // Idle inhibit held by the calling connection (ADR 0051).
        "set_idle_inhibit" => set_idle_inhibit(state, conn, params),
        // Session lock state, and running the locker (ADR 0099). No unlock:
        // only the locker's `unlock_and_destroy` unlocks.
        "get_lock_state" => {
            only_keys(params, &[])?;
            Ok(crate::protocols::standard::session_lock::lock_payload(state))
        }
        "lock_session" => lock_session(state, params),
        // The bar's start menu, relayed on the `launcher` event stream.
        "open_launcher" => open_launcher(state, params),
        // The bar's power drawer, relayed on the `power` event stream.
        "open_power" => open_power(state, params),
        // Agent lifecycle from the human seat (COMP-08, COMP-10 §3.3).
        "get_agents" => get_agents(state),
        "pause_agent" => pause_agent(state, params),
        "resume_agent" => resume_agent(state, params),
        "terminate_agent" => terminate_agent(state, params),
        "revoke_grants" => revoke_grants(state, params),
        // Console wave (A-08 §7, B3): the decision queue and agent install.
        "show_decisions" => show_decisions(state, params),
        "secrets_unlock_prompt" => {
            only_keys(params, &[])?;
            crate::trusted_ui::broker::prompt(state);
            Ok(json!({}))
        }
        "agent_install" => agent_install(state, params),
        // ADR 0084: the effective scheme, theme name and seeds.
        "get_theme" => query(state, Query::GetTheme, |s| {
            only_keys(params, &[])?;
            Ok(crate::theme::payload(s))
        }),
        // Config read/write (COMP-13 §1.4). The outer gate already returned
        // `Allow` to reach this line; `config_rpc` tightens onto it per file.
        "get_config"
        | "set_config_value"
        | "set_config_values"
        | "set_config_collection"
        | "validate_config"
        | "review_widget" => super::config_rpc::dispatch(state, super::gate::Decision::Allow, method, params),
        // Unreachable: the gate rejects anything not in the table and
        // `handle_line` rejects anything the table marks unimplemented.
        other => Err(RpcError::not_implemented(other)),
    }
}

// ------------------------------------------------------- scripted input

fn scripted(state: &mut AbyssState, f: impl FnOnce(&mut AbyssState) -> Reply) -> Reply {
    state.with_origin(crate::input::Origin::Scripted, f)
}

fn f64_param(params: &Value, name: &str) -> Result<f64, RpcError> {
    params_obj(params)
        .get(name)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| RpcError::invalid_params(&format!("{name} must be a number")))
}

fn click_at(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["x", "y", "button", "count"])?;
    let (x, y) = (f64_param(params, "x")?, f64_param(params, "y")?);
    let button = match params_obj(params)
        .get("button")
        .and_then(Value::as_str)
        .unwrap_or("left")
    {
        "left" => 0x110,
        "right" => 0x111,
        "middle" => 0x112,
        _ => return Err(RpcError::invalid_params("button is left, right or middle")),
    };
    let count = match params_obj(params).get("count") {
        None => 1,
        Some(v) => v
            .as_u64()
            .filter(|c| (1..=3).contains(c))
            .ok_or_else(|| RpcError::invalid_params("count is 1 to 3"))? as u32,
    };
    state.inject_click((x, y).into(), button, count);
    Ok(json!({}))
}

fn scroll_at(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["x", "y", "dx", "dy"])?;
    let (x, y) = (f64_param(params, "x")?, f64_param(params, "y")?);
    let dx = f64_param(params, "dx").unwrap_or(0.0);
    let dy = f64_param(params, "dy").unwrap_or(0.0);
    if dx.abs() > 100.0 || dy.abs() > 100.0 {
        return Err(RpcError::invalid_params("dx and dy are at most 100 detents"));
    }
    state.inject_scroll((x, y).into(), dx, dy);
    Ok(json!({}))
}

/// Press at `(x, y)`, move to `(to_x, to_y)` in `steps` (default 8, at most
/// 200), release (COMP-13 §2.2). `button` is left unless named; `modifiers` are
/// held for the whole drag. With `hold_ms` (at most 30000) the release comes
/// from a timer after the last move instead of in this call, so a screenshot
/// can be taken mid-drag; the loop is never blocked.
fn drag_at(state: &mut AbyssState, params: &Value) -> Reply {
    // Refused up front, before a modifier or the button goes down.
    refuse_if_locked(state)?;
    only_keys(
        params,
        &[
            "x",
            "y",
            "to_x",
            "to_y",
            "button",
            "steps",
            "modifiers",
            "hold_ms",
        ],
    )?;
    let from = (f64_param(params, "x")?, f64_param(params, "y")?);
    let to = (f64_param(params, "to_x")?, f64_param(params, "to_y")?);
    let obj = params_obj(params);
    let button = match obj.get("button").and_then(Value::as_str).unwrap_or("left") {
        "left" => 0x110,
        "right" => 0x111,
        "middle" => 0x112,
        _ => return Err(RpcError::invalid_params("button is left, right or middle")),
    };
    let steps = match obj.get("steps") {
        None => 8,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=200).contains(n))
            .ok_or_else(|| RpcError::invalid_params("steps is 1 to 200"))? as u32,
    };
    let hold_ms = match obj.get("hold_ms") {
        None => 0,
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= 30_000)
            .ok_or_else(|| RpcError::invalid_params("hold_ms is 0 to 30000"))?,
    };
    let mods: Vec<&str> = match obj.get("modifiers") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .and_then(|a| a.iter().map(Value::as_str).collect())
            .ok_or_else(|| RpcError::invalid_params("modifiers must be an array of strings"))?,
    };
    state
        .inject_drag(from.into(), to.into(), button, steps, &mods, hold_ms)
        .map_err(RpcError::invalid_params)?;
    Ok(json!({"ok": true, "holding": state.drag_hold.is_some()}))
}

/// The text is never logged or echoed back, only counted.
fn type_text(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["text"])?;
    let text = params_obj(params)
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("text must be a string"))?;
    if text.chars().count() > 4096 {
        return Err(RpcError::invalid_params("text is at most 4096 characters"));
    }
    let typed = state.inject_text(text).map_err(RpcError::invalid_params)?;
    Ok(json!({"typed": typed}))
}

fn press_key(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["key", "modifiers"])?;
    let key = params_obj(params)
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("key must be a keysym name"))?;
    let mods: Vec<&str> = match params_obj(params).get("modifiers") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .and_then(|a| a.iter().map(Value::as_str).collect())
            .ok_or_else(|| RpcError::invalid_params("modifiers must be an array of strings"))?,
    };
    state.inject_key(key, &mods).map_err(RpcError::invalid_params)?;
    Ok(json!({}))
}

// ---------------------------------------------------------------- helpers

fn params_obj(params: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    params.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn u64_param(params: &Value, name: &str) -> Result<u64, RpcError> {
    params_obj(params)
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::invalid_params(&format!("{name} must be a positive integer")))
}

fn bool_param(params: &Value, name: &str) -> Result<bool, RpcError> {
    params_obj(params)
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| RpcError::invalid_params(&format!("{name} must be a boolean")))
}

/// Fail-closed parameter checking: a key we do not know is a caller that
/// thinks it asked for something. Refuse rather than silently ignore it.
fn only_keys(params: &Value, allowed: &[&str]) -> Result<(), RpcError> {
    for key in params_obj(params).keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(RpcError::invalid_params(&format!("unknown parameter: {key}")));
        }
    }
    Ok(())
}

/// An optional positive pixel extent.
fn opt_dimension(params: &Value, name: &str) -> Result<Option<i32>, RpcError> {
    match params_obj(params).get(name) {
        None => Ok(None),
        Some(v) => match v.as_i64() {
            Some(n) if n > 0 && n <= i32::MAX as i64 => Ok(Some(n as i32)),
            _ => Err(RpcError::invalid_params(&format!(
                "{name} must be a positive integer number of logical pixels"
            ))),
        },
    }
}

fn window_param(state: &AbyssState, params: &Value) -> Result<Window, RpcError> {
    let handle = u64_param(params, "handle")?;
    state
        .ipc
        .window_for(handle)
        .filter(IsAlive::alive)
        .ok_or_else(|| RpcError::invalid_params("no such window handle"))
}

/// `app_id` and `title` for a toplevel. Read here and returned to the owner;
/// never written to the journal (COMP-13 §2, ADR 0028).
///
/// An X11 window has no xdg state: its `app_id` is the WM_CLASS class and its
/// title the `_NET_WM_NAME`/`WM_NAME` XWayland already tracks.
pub(crate) fn identity_of(window: &Window) -> (Option<String>, Option<String>) {
    if let Some(x) = window.x11_surface() {
        let some = |s: String| (!s.is_empty()).then_some(s);
        return (some(x.class()), some(x.title()));
    }
    let Some(surface) = crate::shell::window_surface(window) else {
        return (None, None);
    };
    with_states(&surface, |states| {
        let Some(data) = states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
        else {
            return (None, None);
        };
        match data.lock() {
            Ok(d) => (d.app_id.clone(), d.title.clone()),
            Err(_) => (None, None),
        }
    })
}

/// The pid behind a window's client, if the surface still has one.
pub(crate) fn pid_of(state: &AbyssState, window: &Window) -> Option<i32> {
    crate::shell::window_surface(window)
        .and_then(|s| s.client())
        .and_then(|c| c.get_credentials(&state.display_handle).ok())
        .map(|c| c.pid)
}

/// Where a window lives, as `(output id, 1-based workspace)`.
pub(crate) fn location_of(state: &AbyssState, window: &Window) -> Option<(u64, usize)> {
    for entry in state.outputs.iter() {
        for (i, ws) in entry.workspaces.iter().enumerate() {
            if ws.all_windows().iter().any(|w| w == window) {
                return Some((entry.id, i + 1));
            }
        }
    }
    None
}

pub(crate) fn is_floating(state: &AbyssState, window: &Window) -> bool {
    state.outputs.iter().any(|e| {
        e.workspaces
            .iter()
            .any(|ws| ws.floating.iter().any(|f| &f.window == window))
    })
}

/// Focus `window` so the focus-relative shell operations act on it. The
/// shell's command surface is focus-based; the socket addresses windows by
/// handle, and this is the bridge.
fn retarget(state: &mut AbyssState, window: &Window) {
    if state.focus.as_ref() != Some(window) {
        crate::shell::focus_window(state, window);
    }
}

// ---------------------------------------------------------------- queries

fn get_workspaces(state: &mut AbyssState) -> Reply {
    crate::contain::fault(Feature::Queries);
    let mut out = Vec::new();
    for entry in state.outputs.iter() {
        for (i, ws) in entry.workspaces.iter().enumerate() {
            out.push(json!({
                "index": i + 1,
                "output": entry.id,
                "output_name": entry.connector,
                "active": i == entry.active,
                "windows": ws.windows().len(),
                // Every window here is the human's. Agent-owned surfaces get
                // a principal once COMP-08 lands; until then claiming one
                // would be a lie the bar would render.
                "owner": "human",
            }));
        }
    }
    Ok(Value::Array(out))
}

/// The desktop grid (COMP-05 §7.1): the human workspaces laid out in
/// `general.desktop-rows` rows. Counts only, never a title or an app id, and
/// only human workspaces: desktops are the same ten workspaces
/// `get_workspaces` lists, not a second model. A read, so it answers in every
/// mode; `directional` says whether `switch_desktop_direction` will act.
///
/// `desktops` totals each desktop over every output; each output also reports
/// its own active desktop and per-desktop counts. A minimized window still
/// lives on its desktop and counts toward it.
fn get_desktop_grid(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &[])?;
    let grid = crate::shell::desktop::Grid::of(state);
    let count = crate::shell::workspace::COUNT;
    let mut total = vec![0usize; count];
    let mut outputs = Vec::new();
    for entry in state.outputs.iter() {
        let mut per = Vec::with_capacity(count);
        for (i, ws) in entry.workspaces.iter().enumerate().take(count) {
            let n = ws.all_windows().len();
            total[i] += n;
            per.push(json!({"index": i + 1, "occupied": n > 0, "windows": n}));
        }
        outputs.push(json!({
            "output": entry.id,
            "name": entry.connector,
            "active": entry.active + 1,
            "desktops": per,
        }));
    }
    let desktops: Vec<Value> = total
        .iter()
        .enumerate()
        .map(|(i, n)| json!({"index": i + 1, "occupied": *n > 0, "windows": n}))
        .collect();
    Ok(json!({
        "rows": grid.rows,
        "columns": grid.columns,
        "count": count,
        "wrap": state.config.general.desktop_wrap,
        "directional": crate::shell::de::stacking(state),
        "outputs": outputs,
        "desktops": desktops,
    }))
}

fn get_outputs(state: &mut AbyssState, params: &Value) -> Reply {
    // Virtual outputs are the compositor's own scaffolding, not something the
    // human plugged in; hidden unless asked for (COMP-13 §2.1).
    let all = params_obj(params)
        .get("all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut out = Vec::new();
    for entry in state.outputs.iter() {
        let virt = entry.kind == crate::outputs::OutputKind::Virtual;
        if virt && !all {
            continue;
        }
        let mode = entry.output.current_mode();
        let position = state.space.output_geometry(&entry.output).map(|g| g.loc);
        out.push(json!({
            "id": entry.id,
            "name": entry.connector,
            "identity": entry.identity,
            "virtual": virt,
            "enabled": entry.enabled,
            "powered": entry.powered,
            "focused": state.outputs.focused().map(|f| f.id) == Some(entry.id),
            "scale": entry.output.current_scale().fractional_scale(),
            "transform": crate::outputs::transform_name(entry.output.current_transform()),
            "mode": mode.map(|m| json!({"width": m.size.w, "height": m.size.h, "refresh": m.refresh})),
            "modes": entry.output.modes().iter().map(|m| json!({"width": m.size.w, "height": m.size.h, "refresh": m.refresh})).collect::<Vec<_>>(),
            "position": position.map(|p| json!({"x": p.x, "y": p.y})),
            "workspaces": entry.workspaces.len(),
            "active_workspace": entry.active + 1,
            "overscan": overscan_json(entry.overscan),
        }));
    }
    Ok(Value::Array(out))
}

/// Windows on the **active workspace of each output**, plus every window
/// minimized anywhere. `state.space` only maps what is currently shown, so a
/// window moved to an inactive workspace leaves the list until that workspace
/// is switched back to; the per-row `workspace` field therefore names where a
/// visible window is, and is never a way to find one hidden that way.
///
/// Minimized windows are the deliberate exception. A taskbar has to keep
/// showing a chip for a window the human sent away — that chip is the only way
/// back — so they are listed with `minimized: true` and no `focused`. The bar
/// still renders one workspace and nothing else; it just knows which of that
/// workspace's windows are on screen.
fn get_windows(state: &mut AbyssState) -> Reply {
    state.ipc.gc();
    let mut windows: Vec<Window> = state.space.elements().cloned().collect();
    for entry in state.outputs.iter() {
        for ws in entry.workspaces.iter() {
            windows.extend(ws.minimized.iter().map(|m| m.window.clone()));
        }
    }
    let mut out = Vec::new();
    for w in windows {
        let handle = state.ipc.handle_for(&w);
        let (app_id, title) = identity_of(&w);
        let (output, workspace) = match location_of(state, &w) {
            Some((o, i)) => (Some(o), Some(i)),
            None => (None, None),
        };
        let sensitive = crate::shell::window_surface(&w)
            .map(|s| state.sensitive.contains(&s))
            .unwrap_or(false);
        out.push(json!({
            "handle": handle,
            "app_id": app_id,
            "title": title,
            // The themed icon name the client set through xdg-toplevel-icon,
            // else null. Advisory chrome, never identity (COMP-06 §1).
            "icon": crate::shell::window_surface(&w)
                .and_then(|s| crate::protocols::standard::toplevel_icon::icon_name(&s)),
            "output": output,
            "workspace": workspace,
            "floating": is_floating(state, &w),
            "minimized": crate::shell::is_minimized(state, &w),
            // Where it is drawn, global logical px; null while minimized or
            // on an inactive workspace (not in the space).
            "rect": state
                .space
                .element_geometry(&w)
                .map(|g| json!({"x": g.loc.x, "y": g.loc.y, "w": g.size.w, "h": g.size.h})),
            // The client's pid, so the owner's own shell can tie a window to
            // the rest of the session's view of that process — a taskbar
            // matching an audio stream to the window that plays it, say.
            // Owner-only socket; never journalled (COMP-13 §2, ADR 0028).
            "pid": pid_of(state, &w),
            "focused": state.focus.as_ref() == Some(&w),
            // The default class for anything not explicitly raised
            // (root invariant: default is `private`); never a claim of less.
            "trust": if sensitive { "secret" } else { "normal" },
            "no_agent": crate::shell::rules::hidden_from_agents(&w),
            // COMP-05 §1. `class_source` is audit/debug provenance for the
            // class above (`shell::rules::CLASS_SOURCE_*`); nothing decides
            // on it.
            "irreversible_capable": crate::shell::rules::irreversible_capable_of(&w),
            "class_source": crate::shell::rules::class_source_of(&w),
            "client_strip": super::client_strips::debug_for(state, &w),
        }));
    }
    Ok(Value::Array(out))
}

fn get_focused(state: &mut AbyssState) -> Reply {
    let seat = state.seat.name().to_owned();
    let window = state.focus.clone();
    let handle = window.as_ref().map(|w| state.ipc.handle_for(w));
    let (app_id, title) = window.as_ref().map(identity_of).unwrap_or((None, None));
    let workspace = state.outputs.focused().map(|e| e.active + 1);
    let output = state.outputs.focused().map(|e| e.id);
    Ok(json!([{
        "seat": seat,
        "window": handle.map(|h| json!({"handle": h, "app_id": app_id, "title": title})),
        "output": output,
        "workspace": workspace,
    }]))
}

fn get_metrics(state: &mut AbyssState) -> Reply {
    let s = state.stats.snapshot();
    Ok(json!({
        "frames": s.frames,
        "samples": s.samples,
        "render_us": {"p50": s.render_p50_us, "p99": s.render_p99_us},
        "submit_us": {"p50": s.submit_p50_us, "p99": s.submit_p99_us},
        "uptime_s": state.start_time.elapsed().as_secs(),
        "outputs": state.outputs.iter().count(),
        "windows": state.space.elements().count(),
        // Protocol counters and the policy-latency histogram are X-02 and
        // COMP-11 respectively; neither exists yet, so neither is reported.
        "policy_latency_us": Value::Null,
    }))
}

fn dump_state(state: &mut AbyssState) -> Reply {
    Ok(json!({
        "outputs": get_outputs(state, &json!({"all": true}))?,
        "workspaces": get_workspaces(state)?,
        "windows": get_windows(state)?,
        "focused": get_focused(state)?,
        "metrics": get_metrics(state)?,
        "config": {
            "sources": state.config.sources.iter().map(|s| s.path.display().to_string()).collect::<Vec<_>>(),
            "scripted_input": state.config.misc.scripted_input,
        },
    }))
}

// ----------------------------------------------------------- subscription

use ec_abyss_wire::events::EVENTS;

fn subscribe(state: &mut AbyssState, conn: u64, params: &Value) -> Reply {
    let wanted: Vec<String> = match params_obj(params).get("events") {
        None => EVENTS.iter().map(|s| (*s).to_owned()).collect(),
        Some(Value::Array(a)) => {
            let mut v = Vec::new();
            for item in a {
                let s = item
                    .as_str()
                    .ok_or_else(|| RpcError::invalid_params("events must be strings"))?;
                if !EVENTS.contains(&s) {
                    return Err(RpcError::invalid_params(&format!("unknown event kind: {s}")));
                }
                v.push(s.to_owned());
            }
            v
        }
        Some(_) => return Err(RpcError::invalid_params("events must be an array")),
    };
    let Some(c) = state.ipc.conn_mut(conn) else {
        return Err(RpcError::invalid_params("connection is gone"));
    };
    c.subs = wanted.clone();
    // ADR 0064: nodes dropped at startup were refused before any client was
    // connected, so the latest `config-error` (startup-ignored, or a later
    // reload-not-applied) is replayed on every `subscribe` that asks for it,
    // repeatedly, for as long as it stands. A clean load clears it.
    if let Some(event) = state.config_error.clone() {
        crate::ipc::emit_to(state, conn, "config-error", event);
    }
    // ADR 0106: `capture` is state too; a toast daemon that starts mid-share
    // must hold from its first toast.
    if crate::protocols::standard::capture_event::announced(state) {
        crate::ipc::emit_to(state, conn, "capture", json!({"active": true}));
    }
    // ADR 0102: a feature a contained panic turned off stays off, so a
    // subscriber that connected later hears about it too.
    for payload in state.contained.payloads() {
        crate::ipc::emit_to(state, conn, "feature-disabled", payload);
    }
    Ok(json!({"subscribed": wanted}))
}

fn unsubscribe(state: &mut AbyssState, conn: u64) -> Reply {
    if let Some(c) = state.ipc.conn_mut(conn) {
        c.subs.clear();
    }
    Ok(json!({"subscribed": []}))
}

// --------------------------------------------------------------- commands

/// Focus cannot move while the session is locked (COMP-06 §1, COMP-04 §7), so the
/// commands that act through focus would hit the pre-lock window. Refuse.
fn refuse_if_locked(state: &AbyssState) -> Result<(), RpcError> {
    if state.lock.locked {
        return Err(RpcError::denied("session is locked"));
    }
    Ok(())
}

fn focus_window(state: &mut AbyssState, params: &Value) -> Reply {
    refuse_if_locked(state)?;
    let w = window_param(state, params)?;
    crate::shell::focus_window(state, &w);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

fn close_window(state: &mut AbyssState, params: &Value) -> Reply {
    let w = window_param(state, params)?;
    match w.underlying_surface() {
        smithay::desktop::WindowSurface::Wayland(t) => t.send_close(),
        smithay::desktop::WindowSurface::X11(x) => {
            if let Err(e) = x.close() {
                return Err(RpcError {
                    code: -32603,
                    message: format!("could not close the window: {e}"),
                });
            }
        }
    }
    Ok(json!({"ok": true}))
}

/// Minimize or restore a window by handle (COMP-13 §2.1, `window.control`).
///
/// Unlike the focus-relative keybinds this addresses the window directly,
/// which is what a taskbar chip needs: the window it names is by definition
/// not the focused one once it has been sent away.
fn set_minimized(state: &mut AbyssState, params: &Value) -> Reply {
    let w = window_param(state, params)?;
    let value = bool_param(params, "minimized")?;
    crate::shell::set_minimized(state, &w, value);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

/// Record where the bar drew a window's taskbar chip so minimize can aim at
/// it. Render-only: nothing moves and hit-testing, focus and `get_tree` never
/// see it. `rect: null` clears the entry.
fn set_window_chip_rect(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["id", "handle", "output", "rect"])?;
    let obj = params_obj(params);
    let key = if obj.contains_key("id") { "id" } else { "handle" };
    let handle = u64_param(params, key)?;
    let window = state
        .ipc
        .window_for(handle)
        .filter(IsAlive::alive)
        .ok_or_else(|| RpcError::invalid_params("no such window handle"))?;
    let name = obj
        .get("output")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("output must be an output name"))?;
    if !state.outputs.iter().any(|e| e.connector == name) {
        return Err(RpcError::invalid_params("no such output"));
    }
    let chip = match obj.get("rect") {
        None => return Err(RpcError::invalid_params("rect must be an object or null")),
        Some(Value::Null) => None,
        Some(r) => Some(crate::ipc::ChipRect {
            output: name.to_owned(),
            rect: parse_chip_rect(r)?,
        }),
    };
    let handle = state.ipc.handle_for(&window);
    state.ipc.set_chip(handle, chip);
    Ok(json!({"ok": true}))
}

fn parse_chip_rect(v: &Value) -> Result<smithay::utils::Rectangle<f64, smithay::utils::Logical>, RpcError> {
    let bad = || RpcError::invalid_params("rect needs finite x, y and positive w, h");
    let f = |k: &str| {
        v.get(k)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .ok_or_else(bad)
    };
    let (x, y, w, h) = (f("x")?, f("y")?, f("w")?, f("h")?);
    if w <= 0.0 || h <= 0.0 {
        return Err(bad());
    }
    Ok(smithay::utils::Rectangle::new((x, y).into(), (w, h).into()))
}

fn move_to_workspace(state: &mut AbyssState, params: &Value) -> Reply {
    refuse_if_locked(state)?;
    let idx = u64_param(params, "workspace")? as usize;
    // A handle is optional: without one this moves the focused window, which
    // is what a keybind-shaped caller expects.
    if params_obj(params).contains_key("handle") {
        let w = window_param(state, params)?;
        retarget(state, &w);
    }
    if state.focus.is_none() {
        return Err(RpcError::invalid_params("no window to move"));
    }
    crate::shell::move_to_workspace(state, idx);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

fn set_floating(state: &mut AbyssState, params: &Value) -> Reply {
    refuse_if_locked(state)?;
    let want = bool_param(params, "floating")?;
    let w = window_param(state, params)?;
    if is_floating(state, &w) == want {
        return Ok(json!({"ok": true, "changed": false}));
    }
    retarget(state, &w);
    crate::shell::toggle_floating(state);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true, "changed": true}))
}

fn switch_workspace(state: &mut AbyssState, params: &Value) -> Reply {
    let idx = u64_param(params, "workspace")? as usize;
    crate::shell::switch_workspace(state, idx);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

/// Step the focused output's desktop along the grid. Under `wm` and `hybrid`
/// there is no desktop grid to walk and nothing changes. At an unwrapped edge
/// (or on a one-cell line) the answer is `changed: false`, not an error.
fn switch_desktop_direction(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["direction"])?;
    let dir = match params_obj(params).get("direction").and_then(Value::as_str) {
        Some("left") => crate::input::Direction::Left,
        Some("right") => crate::input::Direction::Right,
        Some("up") => crate::input::Direction::Up,
        Some("down") => crate::input::Direction::Down,
        _ => return Err(RpcError::invalid_params("direction is left, right, up or down")),
    };
    refuse_unless_de(state, "switch_desktop_direction")?;
    let changed = crate::shell::desktop::switch(state, dir);
    if changed {
        crate::backend::damage_all(state);
    }
    let active = state.outputs.focused().map(|e| e.active + 1);
    Ok(json!({"ok": true, "changed": changed, "workspace": active}))
}

fn refuse_unless_de(state: &AbyssState, method: &str) -> Result<(), RpcError> {
    if crate::shell::de::stacking(state) {
        Ok(())
    } else {
        Err(RpcError::denied(&format!(
            "{method} is de only (mode \"de\" is not running)"
        )))
    }
}

/// Open the window menu for window `id` (`handle` is accepted too) with its
/// corner at `(x, y)`, global logical px (COMP-05 §7.1). The anchor is clamped
/// onto the window's own output; the menu then keeps itself inside that
/// output's usable area. A minimized window is allowed (the taskbar chip opens
/// it) but only from an anchor on its own output, since its chip cannot be on
/// another. Unknown window, a window not on an output, and a minimized window
/// asked about from another output are `invalid params`; the session lock and
/// any other mode than `de` are `denied`.
fn open_window_menu(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["id", "handle", "x", "y"])?;
    refuse_unless_de(state, "open_window_menu")?;
    refuse_if_locked(state)?;
    let obj = params_obj(params);
    let key = if obj.contains_key("id") { "id" } else { "handle" };
    let handle = u64_param(params, key)?;
    let anchor = (f64_param(params, "x")?, f64_param(params, "y")?);
    let window = state
        .ipc
        .window_for(handle)
        .filter(IsAlive::alive)
        .ok_or_else(|| RpcError::invalid_params("no such window"))?;
    let id = crate::shell::output_of_window(state, &window)
        .ok_or_else(|| RpcError::invalid_params("window is not on an output"))?;
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    let geo = state
        .space
        .output_geometry(&output)
        .ok_or_else(|| RpcError::invalid_params("window's output has no geometry"))?;
    let on_it = |p: (f64, f64)| geo.to_f64().contains(smithay::utils::Point::from(p));
    if crate::shell::is_minimized(state, &window) && !on_it(anchor) {
        return Err(RpcError::invalid_params(
            "window is minimized on another output than the anchor",
        ));
    }
    let clamp =
        |v: f64, lo: i32, len: i32| v.clamp(f64::from(lo), f64::from(lo + len.max(1) - 1)).round() as i32;
    let at = smithay::utils::Point::from((
        clamp(anchor.0, geo.loc.x, geo.size.w),
        clamp(anchor.1, geo.loc.y, geo.size.h),
    ));
    if !crate::shell::winmenu::open_window_menu(state, &window, at) {
        return Err(RpcError::denied("the window menu cannot open now"));
    }
    Ok(json!({"ok": true, "x": at.x, "y": at.y}))
}

/// Show the hover preview of window `id` (`handle` is accepted too) near the
/// taskbar chip at `(x, y)` sized `width` x `height`, global logical px
/// (COMP-05 §7.1). The card is placed beside the rectangle and kept on its
/// output; it needs no input, and the bar hides it again with
/// `hide_window_preview`. It also ends when the window closes, when focus moves
/// to the window, on the session lock, and after ten idle seconds.
///
/// `{"ok": true, "shown": true}` when it is up. `shown: false` when the
/// window must have no card at all (it is protected, COMP-19): the call is
/// answered, nothing is drawn. Unknown window, a window on no output and a bad
/// rectangle are `invalid params`; any other mode than `de`, the session lock
/// and an open switcher or overview are `denied`. A redacted window shows a
/// blank card with no title.
fn show_window_preview(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["id", "handle", "x", "y", "width", "height"])?;
    refuse_unless_de(state, "show_window_preview")?;
    refuse_if_locked(state)?;
    let obj = params_obj(params);
    let key = if obj.contains_key("id") { "id" } else { "handle" };
    let handle = u64_param(params, key)?;
    let extent = |name: &str| {
        let v = f64_param(params, name)?;
        if (0.0..=100_000.0).contains(&v) {
            Ok(v.round() as i32)
        } else {
            Err(RpcError::invalid_params(&format!("{name} is out of range")))
        }
    };
    let coord = |name: &str| {
        let v = f64_param(params, name)?;
        if v.abs() <= 100_000.0 {
            Ok(v.round() as i32)
        } else {
            Err(RpcError::invalid_params(&format!("{name} is out of range")))
        }
    };
    let (w, h) = (extent("width")?, extent("height")?);
    if w == 0 || h == 0 {
        return Err(RpcError::invalid_params("width and height must be positive"));
    }
    let anchor = smithay::utils::Rectangle::new(
        smithay::utils::Point::from((coord("x")?, coord("y")?)),
        smithay::utils::Size::from((w, h)),
    );
    let window = state
        .ipc
        .window_for(handle)
        .filter(IsAlive::alive)
        .ok_or_else(|| RpcError::invalid_params("no such window"))?;
    if crate::shell::output_of_window(state, &window).is_none() {
        return Err(RpcError::invalid_params("window is not on an output"));
    }
    let shown = crate::overview::preview::show(state, &window, anchor).map_err(RpcError::denied)?;
    Ok(json!({"ok": true, "shown": shown}))
}

/// Take the hover preview down. `{"ok": true, "hidden": true}` when one was
/// up; with none it is still `ok`.
fn hide_window_preview(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &[])?;
    let hidden = crate::overview::preview::hide(state);
    Ok(json!({"ok": true, "hidden": hidden}))
}

fn reload_config(state: &mut AbyssState) -> Reply {
    crate::config::watch::reload_now(state);
    Ok(json!({
        "ok": true,
        "sources": state.config.sources.iter().map(|s| s.path.display().to_string()).collect::<Vec<_>>(),
    }))
}

/// Absolute logical-pixel resize (COMP-13 §2.1). `handle` is optional and
/// defaults to the focused window, like `move_to_workspace`. Tiled and
/// floating are different operations and `shell::resize_window` picks; under
/// the master layout a tiled window has no per-window size and the call is
/// refused rather than silently ignored (ADR 0031).
fn resize(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["handle", "width", "height"])?;
    let width = opt_dimension(params, "width")?;
    let height = opt_dimension(params, "height")?;
    if width.is_none() && height.is_none() {
        return Err(RpcError::invalid_params("width or height is required"));
    }
    let window = if params_obj(params).contains_key("handle") {
        window_param(state, params)?
    } else {
        state
            .focus
            .clone()
            .ok_or_else(|| RpcError::invalid_params("no window to resize"))?
    };
    let changed =
        crate::shell::resize_window(state, &window, width, height).map_err(RpcError::invalid_params)?;
    if changed {
        let handle = state.ipc.handle_for(&window);
        super::emit(state, "window", json!({"change": "resized", "handle": handle}));
        crate::backend::damage_all(state);
    }
    Ok(json!({"ok": true, "changed": changed}))
}

/// Hand a workspace's windows to another output (COMP-13 §2.1). `output` is
/// the destination; the source defaults to the focused output. Everything is
/// validated before anything moves.
fn move_workspace_to_output(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["workspace", "output", "from"])?;
    let idx = u64_param(params, "workspace")? as usize;
    let to = u64_param(params, "output")?;
    let from = match params_obj(params).get("from") {
        Some(_) => u64_param(params, "from")?,
        None => state
            .outputs
            .focused()
            .map(|e| e.id)
            .ok_or_else(|| RpcError::invalid_params("no focused output"))?,
    };
    let moved =
        crate::shell::move_workspace_to_output(state, from, idx, to).map_err(RpcError::invalid_params)?;
    if moved {
        super::emit(
            state,
            "workspace",
            json!({"change": "moved", "workspace": idx, "from": from, "output": to}),
        );
        crate::backend::damage_all(state);
    }
    Ok(json!({"ok": true, "changed": moved}))
}

/// One overscan edge, in physical pixels. Non-negative and sane; the caller
/// clamps to a fraction of the axis, which needs the mode and so happens later.
fn edge_px(raw: Option<i64>) -> Result<i32, RpcError> {
    match raw {
        Some(n) if (0..=10_000).contains(&n) => Ok(n as i32),
        _ => Err(RpcError::invalid_params(
            "overscan pixels must be an integer in [0, 10000]",
        )),
    }
}

/// `overscan` on the wire is either a single number for all four edges or an
/// object with any subset of the edges; the omitted ones stay at zero, matching
/// the config spelling exactly so the two front ends cannot drift.
fn parse_overscan(
    obj: &serde_json::Map<String, Value>,
) -> Result<Option<crate::outputs::overscan::Overscan>, RpcError> {
    match obj.get("overscan") {
        None => Ok(None),
        Some(Value::Number(n)) => {
            let px = edge_px(n.as_i64())?;
            Ok(Some(crate::outputs::overscan::Overscan::uniform(px)))
        }
        Some(Value::Object(o)) => {
            for key in o.keys() {
                if !["top", "bottom", "left", "right"].contains(&key.as_str()) {
                    return Err(RpcError::invalid_params(&format!("unknown overscan edge: {key}")));
                }
            }
            let mut v = crate::outputs::overscan::Overscan::default();
            for (key, slot) in [
                ("top", &mut v.top),
                ("bottom", &mut v.bottom),
                ("left", &mut v.left),
                ("right", &mut v.right),
            ] {
                if let Some(raw) = o.get(key) {
                    *slot = edge_px(raw.as_i64())?;
                }
            }
            Ok(Some(v))
        }
        Some(_) => Err(RpcError::invalid_params(
            "overscan must be a number or {top, bottom, left, right}",
        )),
    }
}

/// The reported shape of an output's overscan. Keyed exactly like the object
/// form `set_output` accepts so a client can read a row and hand it straight
/// back; always present, all-zero when there is no compensation.
fn overscan_json(o: crate::outputs::overscan::Overscan) -> Value {
    json!({"top": o.top, "right": o.right, "bottom": o.bottom, "left": o.left})
}

/// Output runtime configuration (COMP-13 §2.1): mode, scale, position,
/// transform, enabled, vrr. Everything is parsed and checked first; the output
/// is touched only once every field is known good, so a rejected request
/// leaves no half-applied state. Geometry changes persist to `outputs.kdl`
/// through the normal `outputs::relayout` save path (COMP-03 §4); `vrr` does
/// not, because the persisted record has no field for it. `overscan` persists
/// on its own per-panel path (COMP-03 §2).
fn set_output(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(
        params,
        &[
            "output",
            "mode",
            "scale",
            "position",
            "transform",
            "enabled",
            "vrr",
            "overscan",
        ],
    )?;
    let id = u64_param(params, "output")?;
    let entry = state
        .outputs
        .get(id)
        .ok_or_else(|| RpcError::invalid_params("no such output"))?;
    let output = entry.output.clone();
    let obj = params_obj(params);

    let mode = match obj.get("mode") {
        None => None,
        Some(Value::String(s)) => {
            let (w, h, r) = crate::outputs::persist::parse_mode(s)
                .ok_or_else(|| RpcError::invalid_params("mode must look like 1920x1080@60"))?;
            let found = output
                .modes()
                .into_iter()
                .find(|m| m.size.w == w && m.size.h == h && m.refresh == r)
                .or_else(|| {
                    output
                        .modes()
                        .into_iter()
                        .find(|m| m.size.w == w && m.size.h == h)
                });
            Some(found.ok_or_else(|| RpcError::invalid_params("the output has no such mode"))?)
        }
        Some(_) => return Err(RpcError::invalid_params("mode must be a string")),
    };
    let scale = match obj.get("scale") {
        None => None,
        Some(v) => match v.as_f64() {
            Some(f) if f > 0.0 && f <= 10.0 => Some(crate::outputs::scale_from(f)),
            _ => return Err(RpcError::invalid_params("scale must be a number in (0, 10]")),
        },
    };
    let transform = match obj.get("transform") {
        None => None,
        Some(Value::String(s)) => Some(
            crate::outputs::parse_transform(s)
                .ok_or_else(|| RpcError::invalid_params("unknown transform"))?,
        ),
        Some(_) => return Err(RpcError::invalid_params("transform must be a string")),
    };
    let position = match obj.get("position") {
        None => None,
        Some(Value::Object(p)) => {
            let coord = |name: &str| {
                p.get(name)
                    .and_then(Value::as_i64)
                    .filter(|n| *n >= i32::MIN as i64 && *n <= i32::MAX as i64)
                    .map(|n| n as i32)
                    .ok_or_else(|| RpcError::invalid_params("position needs integer x and y"))
            };
            Some((coord("x")?, coord("y")?))
        }
        Some(_) => return Err(RpcError::invalid_params("position must be {x, y}")),
    };
    let enabled = match obj.get("enabled") {
        None => None,
        Some(_) => Some(bool_param(params, "enabled")?),
    };
    let vrr = match obj.get("vrr") {
        None => None,
        Some(_) => Some(bool_param(params, "vrr")?),
    };
    let overscan = parse_overscan(obj)?;

    // --- everything validated; hand off to the shared apply path.
    let change = crate::outputs::OutputChange {
        overscan,
        mode,
        scale,
        transform,
        position,
        enabled,
        vrr,
    };
    let vrr_applied = crate::outputs::apply_change(state, id, &change).map_err(RpcError::invalid_params)?;
    Ok(json!({"ok": true, "vrr": vrr_applied}))
}

/// Drive the overscan calibration overlay on one output (COMP-03 §2).
///
/// `action` defaults to `start`. The overlay owns the seat while it runs, so
/// `commit` and `cancel` take no output: there is only ever one session, and
/// naming an output that is not the one calibrating would be a lie either way.
fn calibrate_output(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["output", "action"])?;
    let action = params_obj(params)
        .get("action")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| RpcError::invalid_params("action must be a string"))
        })
        .transpose()?
        .unwrap_or("start");

    match action {
        "start" => {
            let id = u64_param(params, "output")?;
            if state.outputs.get(id).is_none() {
                return Err(RpcError::invalid_params("no such output"));
            }
            if !crate::outputs::calibrate::start(state, id) {
                return Err(RpcError::invalid_params("a calibration is already running"));
            }
            Ok(json!({"ok": true, "calibrating": id}))
        }
        "commit" | "cancel" => {
            let Some(id) = crate::outputs::calibrate::active(state) else {
                return Err(RpcError::invalid_params("no calibration is running"));
            };
            if action == "commit" {
                crate::outputs::calibrate::commit(state);
            } else {
                crate::outputs::calibrate::cancel(state);
            }
            Ok(json!({"ok": true, "output": id}))
        }
        other => Err(RpcError::invalid_params(&format!(
            "unknown action {other:?}: expected start, commit or cancel"
        ))),
    }
}

// ----------------------------------------------------------- annotations
//
// COMP-18 §3, ADR 0054. The caller sends an optional rectangle (absent:
// unanchored, drawn top-centre), an optional kind (`answer`, the default, or
// `error`), a body, an optional
// one-line title and an optional pick (a labelled sub-rectangle marking the
// suggested option) and gets a handle. Everything else -- sanitising,
// wrapping, styling, placement, eviction, clamping the pick and dropping one
// that is not wholly inside the anchor -- belongs to `render::annotation`, so
// that a caller can affect nothing but the glyphs and one highlight within
// its own anchor. These handlers therefore do parameter checking and nothing
// more: a pick outside the anchor is not an IPC error, it is simply not drawn.

fn rect_param(params: &Value) -> Result<Rectangle<i32, Logical>, RpcError> {
    let obj = params_obj(params);
    let mut v = [0i32; 4];
    for (slot, name) in v.iter_mut().zip(["x", "y", "w", "h"]) {
        *slot = obj
            .get(name)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| RpcError::invalid_params(&format!("{name} must be an integer")))?;
    }
    if v[2] <= 0 || v[3] <= 0 {
        return Err(RpcError::invalid_params("w and h must be positive"));
    }
    Ok(Rectangle::new((v[0], v[1]).into(), (v[2], v[3]).into()))
}

fn text_param(params: &Value) -> Result<&str, RpcError> {
    params_obj(params)
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("text must be a string"))
}

/// Optional title; absent means empty.
fn title_param(params: &Value) -> Result<&str, RpcError> {
    match params_obj(params).get("title") {
        None => Ok(""),
        Some(v) => v
            .as_str()
            .ok_or_else(|| RpcError::invalid_params("title must be a string")),
    }
}

/// Most picks one `annotation_create` may carry (ADR 0054, `picks`).
const MAX_PICKS: usize = crate::render::annotation::MAX_PICKS;

/// One pick `{x, y, w, h, label}`. Unknown keys inside it are refused, same as
/// at the top level. Used for both `pick` and each entry of `picks`.
fn pick_value(pick: &Value) -> Result<crate::render::annotation::Pick, RpcError> {
    only_keys(pick, &["x", "y", "w", "h", "label"])
        .map_err(|_| RpcError::invalid_params("pick takes only x, y, w, h and label"))?;
    let rect = rect_param(pick)
        .map_err(|_| RpcError::invalid_params("pick.x/y/w/h must be integers with w and h positive"))?;
    let label = params_obj(pick)
        .get("label")
        .and_then(Value::as_str)
        .and_then(crate::render::annotation::pick_label)
        .ok_or_else(|| RpcError::invalid_params("pick.label must be 1-2 ASCII letters or digits"))?;
    Ok(crate::render::annotation::Pick { rect, label })
}

/// Optional picks: the legacy single `pick {…}`, or `picks [{…}, …]` of at
/// most [`MAX_PICKS`]. `null` counts as absent; both present is refused.
fn picks_param(params: &Value) -> Result<Vec<crate::render::annotation::Pick>, RpcError> {
    let obj = params_obj(params);
    let present = |k: &str| obj.get(k).filter(|v| !v.is_null());
    match (present("pick"), present("picks")) {
        (Some(_), Some(_)) => Err(RpcError::invalid_params("give pick or picks, not both")),
        (Some(p @ Value::Object(_)), None) => Ok(vec![pick_value(p)?]),
        (Some(_), None) => Err(RpcError::invalid_params("pick must be an object")),
        (None, Some(Value::Array(items))) => {
            if items.len() > MAX_PICKS {
                return Err(RpcError::invalid_params("picks takes at most 16 entries"));
            }
            items
                .iter()
                .map(|p| match p {
                    Value::Object(_) => pick_value(p),
                    _ => Err(RpcError::invalid_params("each picks entry must be an object")),
                })
                .collect()
        }
        (None, Some(_)) => Err(RpcError::invalid_params("picks must be an array")),
        (None, None) => Ok(Vec::new()),
    }
}

/// Optional anchor: none of `x/y/w/h` present means unanchored (drawn
/// top-centre by `render::annotation`); any one present means all four are
/// required and checked exactly as before.
fn anchor_param(params: &Value) -> Result<Option<Rectangle<i32, Logical>>, RpcError> {
    let obj = params_obj(params);
    if ["x", "y", "w", "h"].iter().all(|k| !obj.contains_key(*k)) {
        return Ok(None);
    }
    rect_param(params).map(Some)
}

/// Optional `kind`: absent means `answer`. Anything but the two known names
/// is refused (fail closed), never mapped to a default.
fn kind_param(params: &Value) -> Result<crate::render::annotation::AnnotationKind, RpcError> {
    use crate::render::annotation::AnnotationKind;
    match params_obj(params).get("kind") {
        None => Ok(AnnotationKind::Answer),
        Some(v) => match v.as_str() {
            Some("answer") => Ok(AnnotationKind::Answer),
            Some("error") => Ok(AnnotationKind::Error),
            _ => Err(RpcError::invalid_params("kind must be \"answer\" or \"error\"")),
        },
    }
}

type CreateParams<'a> = (
    Option<Rectangle<i32, Logical>>,
    crate::render::annotation::AnnotationKind,
    &'a str,
    &'a str,
    Vec<crate::render::annotation::Pick>,
);

fn create_params(params: &Value) -> Result<CreateParams<'_>, RpcError> {
    only_keys(
        params,
        &["x", "y", "w", "h", "kind", "text", "title", "pick", "picks"],
    )?;
    let anchor = anchor_param(params)?;
    let kind = kind_param(params)?;
    let title = title_param(params)?;
    let text = text_param(params)?;
    let picks = picks_param(params)?;
    // A pick marks a sub-rectangle of the anchor; with no anchor there is
    // nothing for it to be inside.
    if !picks.is_empty() && anchor.is_none() {
        return Err(RpcError::invalid_params("pick requires x, y, w and h"));
    }
    Ok((anchor, kind, title, text, picks))
}

fn update_params(params: &Value) -> Result<(u64, &str, &str), RpcError> {
    only_keys(params, &["id", "text", "title"])?;
    Ok((
        u64_param(params, "id")?,
        title_param(params)?,
        text_param(params)?,
    ))
}

fn annotation_create(state: &mut AbyssState, conn: u64, params: &Value) -> Reply {
    let (anchor, kind, title, text, picks) = create_params(params)?;
    let Some(id) = state
        .annotations
        .create_picks(conn, anchor, kind, title, text, picks)
    else {
        return Err(RpcError::invalid_params("too many live annotations"));
    };
    crate::contain::fault(Feature::Annotations);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true, "id": id.0}))
}

fn annotation_update(state: &mut AbyssState, conn: u64, params: &Value) -> Reply {
    // An update replaces the title too: omitting it clears it.
    let (id, title, text) = update_params(params)?;
    let id = crate::render::annotation::AnnotationId(id);
    if !state.annotations.update(conn, id, title, text) {
        return Err(RpcError::invalid_params("no such annotation"));
    }
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

fn annotation_destroy(state: &mut AbyssState, conn: u64, params: &Value) -> Reply {
    only_keys(params, &["id"])?;
    let id = crate::render::annotation::AnnotationId(u64_param(params, "id")?);
    if !state.annotations.destroy(conn, id) {
        return Err(RpcError::invalid_params("no such annotation"));
    }
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

fn annotation_clear(state: &mut AbyssState, conn: u64, params: &Value) -> Reply {
    only_keys(params, &[])?;
    let dropped = state.annotations.clear_for(conn);
    if dropped > 0 {
        crate::backend::damage_all(state);
    }
    Ok(json!({"ok": true, "dropped": dropped}))
}

/// Hold or release an idle inhibit on behalf of the calling connection. The
/// caller states the level; the compositor drops it if the connection dies.
fn set_idle_inhibit(state: &mut AbyssState, conn: u64, params: &Value) -> Reply {
    only_keys(params, &["inhibit"])?;
    let inhibit = bool_param(params, "inhibit")?;
    state.idle.set_conn_inhibit(conn, inhibit);
    Ok(json!({"ok": true, "inhibit": inhibit}))
}

/// Run `idle.lock-command` (ADR 0099), as the idle lock timeout does. A
/// session held by a live locker answers `spawned: false`; a locked session
/// whose locker died gets a fresh one, which takes it over. This can only
/// ever lock: there is no unlock method.
fn lock_session(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &[])?;
    if state.lock.locker_alive() {
        return Ok(json!({"ok": true, "spawned": false}));
    }
    if state.kiosk {
        return Err(RpcError::denied("no session to lock in kiosk mode"));
    }
    if state.config.idle.lock_command.is_none() {
        return Err(RpcError::invalid_params("no idle.lock-command is configured"));
    }
    let spawned = crate::protocols::standard::session_lock::spawn_locker(state, "control socket");
    Ok(json!({"ok": true, "spawned": spawned}))
}

/// Ask the bar to open its start menu (`launcher.style "menu"`,
/// COMP-13 §2.1). `output` is an output id as `get_outputs` reports it;
/// absent or null means the focused output, so exactly one bar opens. The
/// compositor keeps no launcher state: it names the output and relays the
/// request as a `launcher` event, `{"action": "open", "output": id,
/// "output_name": connector}`. `delivered` says whether anything was
/// subscribed to hear it, so a caller with no bar running can fall back to
/// the centred launcher instead of doing nothing.
fn open_launcher(state: &mut AbyssState, params: &Value) -> Reply {
    relay_open(state, params, "launcher")
}

/// Ask the bar to open its power drawer (the short power-key press). Same
/// shape and rules as [`open_launcher`], relayed as a `power` event.
fn open_power(state: &mut AbyssState, params: &Value) -> Reply {
    relay_open(state, params, "power")
}

fn relay_open(state: &mut AbyssState, params: &Value, event: &'static str) -> Reply {
    only_keys(params, &["output"])?;
    let entry = match params_obj(params).get("output") {
        None | Some(Value::Null) => state
            .outputs
            .focused()
            .ok_or_else(|| RpcError::invalid_params("no output is attached"))?,
        Some(_) => state
            .outputs
            .get(u64_param(params, "output")?)
            .ok_or_else(|| RpcError::invalid_params("no such output"))?,
    };
    let (id, name) = (entry.id, entry.connector.clone());
    let delivered = super::has_subscriber(state, event);
    super::emit(
        state,
        event,
        json!({"action": "open", "output": id, "output_name": name}),
    );
    Ok(json!({"ok": true, "output": id, "output_name": name, "delivered": delivered}))
}

// ------------------------------------------------------- agent lifecycle
//
// The decisions live in `policy::lifecycle` (TCB); these arms only parse and
// name the agent. They mirror the emergency panel and are `Privileged`, so
// the gate has already required the socket owner's uid.

/// `get_agents`: every live agent object, its state and grant summary.
///
/// The spec also lists a "recent action count". That is audit data; abyss
/// does not hold the audit store and the emergency panel gets it from
/// policyd (VOL1 5226), so it is omitted here.
fn get_agents(state: &mut AbyssState) -> Reply {
    use crate::policy::lifecycle::{all_paused, is_paused};
    let agents: Vec<Value> = state
        .agents
        .list()
        .into_iter()
        .map(|(id, principal)| {
            json!({
                "id": id,
                "principal": principal,
                "task": state.agents.task_of(id).map(|t| t.to_string()),
                "state": if is_paused(state, id) { "paused" } else { "running" },
                "grants": state.agents.grants_of(id),
            })
        })
        .collect();
    Ok(json!({"agents": agents, "all_paused": all_paused(state)}))
}

/// The `id` parameter, which must name a live agent object.
fn agent_param(state: &AbyssState, params: &Value) -> Result<u64, RpcError> {
    let id = u64_param(params, "id")?;
    if state.agents.principal_of(id).is_none() {
        return Err(RpcError::invalid_params(&format!("no such agent: {id}")));
    }
    Ok(id)
}

/// `{"id":N}` or `{"all":true}`; `None` means every agent.
fn agent_or_all(state: &AbyssState, params: &Value) -> Result<Option<u64>, RpcError> {
    only_keys(params, &["id", "all"])?;
    let p = params_obj(params);
    match (p.contains_key("id"), p.contains_key("all")) {
        (true, true) => Err(RpcError::invalid_params("give id or all, not both")),
        (false, true) => {
            if bool_param(params, "all")? {
                Ok(None)
            } else {
                Err(RpcError::invalid_params("all must be true"))
            }
        }
        _ => agent_param(state, params).map(Some),
    }
}

fn pause_agent(state: &mut AbyssState, params: &Value) -> Reply {
    match agent_or_all(state, params)? {
        Some(id) => crate::policy::lifecycle::pause(state, id),
        None => crate::policy::lifecycle::pause_all(state),
    }
    Ok(json!({}))
}

fn resume_agent(state: &mut AbyssState, params: &Value) -> Reply {
    match agent_or_all(state, params)? {
        Some(id) => crate::policy::lifecycle::resume(state, id),
        None => crate::policy::lifecycle::resume_all(state),
    }
    Ok(json!({}))
}

fn terminate_agent(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["id"])?;
    let id = agent_param(state, params)?;
    crate::policy::lifecycle::terminate(state, id);
    Ok(json!({}))
}

fn revoke_grants(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &["id"])?;
    let id = agent_param(state, params)?;
    crate::policy::lifecycle::revoke(state, id);
    Ok(json!({}))
}

// ------------------------------------------------------- console wave

/// Minimum spacing between `show_decisions` calls (A-08 §7: 1/s).
const SHOW_DECISIONS_MIN_GAP: std::time::Duration = std::time::Duration::from_secs(1);

/// `show_decisions`: ask the compositor to open the decision queue
/// (COMP-10 §3.13). Carries no decision content and answers `{}`; the queue
/// itself is trusted UI and opens with Deny focused.
fn show_decisions(state: &mut AbyssState, params: &Value) -> Reply {
    only_keys(params, &[])?;
    show_decisions_at(state, std::time::Instant::now())
}

fn show_decisions_at(state: &mut AbyssState, now: std::time::Instant) -> Reply {
    if let Some(last) = state.ipc.last_show_decisions {
        let since = now.saturating_duration_since(last);
        if since < SHOW_DECISIONS_MIN_GAP {
            let retry = (SHOW_DECISIONS_MIN_GAP - since).as_millis() as u64;
            return Err(RpcError::rate_limited(retry));
        }
    }
    state.ipc.last_show_decisions = Some(now);
    super::hooks::open_decision_queue(state);
    Ok(json!({}))
}

/// Validate the `path` of `agent_install`: absolute, an existing directory,
/// returned canonicalised (symlinks and `..` resolved) so what policyd is
/// handed is the directory that was checked.
fn install_path(params: &Value) -> Result<std::path::PathBuf, RpcError> {
    only_keys(params, &["path"])?;
    let raw = params_obj(params)
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("path must be a string"))?;
    if raw.is_empty() || raw.contains('\0') {
        return Err(RpcError::invalid_params("path must be a non-empty path"));
    }
    let p = std::path::Path::new(raw);
    if !p.is_absolute() {
        return Err(RpcError::invalid_params("path must be absolute"));
    }
    let canon = std::fs::canonicalize(p)
        .map_err(|_| RpcError::invalid_params("path is not an existing directory"))?;
    if !canon.is_dir() {
        return Err(RpcError::invalid_params("path is not an existing directory"));
    }
    Ok(canon)
}

/// `agent_install {path}`: begin installing an agent package from a
/// directory. This only hands the path on; the review is a trusted modal and
/// nothing is installed without the human's answer there. Privileged, so the
/// gate has already required the owner uid.
fn agent_install(state: &mut AbyssState, params: &Value) -> Reply {
    let path = install_path(params)?;
    match super::hooks::install_begin(state, &path) {
        Ok(()) => Ok(json!({})),
        Err(why) => Err(RpcError::denied(why)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 0099: the lock methods' shapes and refusals.
    #[test]
    fn get_lock_state_and_lock_session() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        assert_eq!(
            dispatch(s, 0, "get_lock_state", &json!({})).ok(),
            Some(json!({"locked": false, "pending": false, "locker_alive": false}))
        );
        assert!(dispatch(s, 0, "get_lock_state", &json!({"x": 1})).is_err());
        let e = dispatch(s, 0, "lock_session", &json!({})).expect_err("no lock-command");
        assert_eq!(e.code, ec_abyss_wire::rpc::INVALID_PARAMS);
        s.config.idle.lock_command = Some("true".into());
        assert_eq!(
            dispatch(s, 0, "lock_session", &json!({})).ok(),
            Some(json!({"ok": true, "spawned": true}))
        );
        let state = dispatch(s, 0, "get_lock_state", &json!({}))
            .ok()
            .expect("lock state");
        assert_eq!(state["pending"], json!(true), "the locker was asked for");
        assert_eq!(state["locked"], json!(false), "running the locker is not locking");
        s.kiosk = true;
        assert!(dispatch(s, 0, "lock_session", &json!({})).is_err());
    }

    #[test]
    fn show_decisions_is_rate_limited_to_one_per_second() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let t0 = std::time::Instant::now();
        assert_eq!(show_decisions_at(s, t0).ok(), Some(json!({})));
        let e = show_decisions_at(s, t0 + std::time::Duration::from_millis(400)).expect_err("limited");
        assert_eq!(e.code, ec_abyss_wire::rpc::RATE_LIMITED);
        assert!(e.message.contains("rate_limited"));
        // A refused call does not restart the window.
        assert!(show_decisions_at(s, t0 + std::time::Duration::from_millis(1001)).is_ok());
        assert!(dispatch(s, 0, "show_decisions", &json!({"x": 1})).is_err());
    }

    #[test]
    fn agent_install_path_validation() {
        let dir = std::env::temp_dir().join(format!("ec-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("f");
        std::fs::write(&file, b"x").expect("write");

        assert!(install_path(&json!({})).is_err());
        assert!(install_path(&json!({"path": 3})).is_err());
        assert!(install_path(&json!({"path": "relative/dir"})).is_err());
        assert!(install_path(&json!({"path": dir.join("missing")})).is_err());
        assert!(install_path(&json!({"path": file})).is_err());
        assert!(install_path(&json!({"path": dir, "extra": 1})).is_err());
        // `..` is resolved, not trusted.
        let dotted = dir.join("..").join(dir.file_name().expect("name"));
        assert_eq!(
            install_path(&json!({"path": dotted})).ok(),
            Some(std::fs::canonicalize(&dir).expect("canon"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The agent lifecycle methods: shapes, state, and unknown ids.
    #[test]
    fn agent_lifecycle_methods() {
        use ec_policy_eval::grant::{cose_sign1, protected_header, sig_structure};
        use ec_policy_eval::{Capability, Constraints, Grant, Ulid};
        use ed25519_dalek::{Signer, SigningKey};

        let k = SigningKey::from_bytes(&[3u8; 32]);
        let g = Grant {
            id: Ulid([1; 16]),
            principal: "agent:test".into(),
            issued_ms: 0,
            expires_ms: u64::MAX,
            issuer: "policyd".into(),
            task_id: Ulid([9; 16]),
            capabilities: vec![Capability {
                name: "scene.list".into(),
                scopes: vec!["workspace:human".into()],
                quota: None,
            }],
            constraints: Constraints::default(),
            unattended: false,
        };
        let protected = protected_header(&k.verifying_key());
        let payload = g.encode();
        let sig = k.sign(&sig_structure(&protected, &payload));
        let cose = cose_sign1(&protected, &payload, &sig.to_bytes());
        let agent =
            crate::policy::Agent::admit(&cose, Some(&k.verifying_key()), 1, |_| false).expect("admit");

        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let id = s.agents.insert_for_test(agent);

        let r = dispatch(s, 0, "get_agents", &Value::Null).ok().expect("list");
        assert_eq!(r["all_paused"], json!(false));
        let a = &r["agents"][0];
        assert_eq!(a["id"], json!(id));
        assert_eq!(a["principal"], json!("agent:test"));
        assert_eq!(a["state"], json!("running"));
        assert_eq!(a["task"], json!(Ulid([9; 16]).to_string()));
        assert!(a["grants"].as_array().is_some_and(|g| !g.is_empty()));

        assert_eq!(
            dispatch(s, 0, "pause_agent", &json!({"id": id})).ok(),
            Some(json!({}))
        );
        let r = dispatch(s, 0, "get_agents", &Value::Null).ok().expect("list");
        assert_eq!(r["agents"][0]["state"], json!("paused"));
        assert!(dispatch(s, 0, "resume_agent", &json!({"id": id})).is_ok());
        let r = dispatch(s, 0, "get_agents", &Value::Null).ok().expect("list");
        assert_eq!(r["agents"][0]["state"], json!("running"));

        assert!(dispatch(s, 0, "pause_agent", &json!({"all": true})).is_ok());
        let r = dispatch(s, 0, "get_agents", &Value::Null).ok().expect("list");
        assert_eq!(r["all_paused"], json!(true));
        assert_eq!(r["agents"][0]["state"], json!("paused"));
        assert!(dispatch(s, 0, "resume_agent", &json!({"all": true})).is_ok());
        assert!(!crate::policy::lifecycle::all_paused(s));

        // Malformed and unknown targets are invalid params, and change nothing.
        for (m, p) in [
            ("pause_agent", json!({"id": 999})),
            ("resume_agent", json!({"id": 999})),
            ("terminate_agent", json!({"id": 999})),
            ("revoke_grants", json!({"id": 999})),
            ("pause_agent", json!({})),
            ("pause_agent", json!({"id": id, "all": true})),
            ("pause_agent", json!({"all": false})),
            ("terminate_agent", json!({"all": true})),
            ("revoke_grants", json!({"id": id, "extra": 1})),
        ] {
            let e = dispatch(s, 0, m, &p).err().unwrap_or_else(|| panic!("{m} {p}"));
            assert_eq!(e.code, super::super::INVALID_PARAMS, "{m} {p}");
        }
        assert!(!crate::policy::lifecycle::is_paused(s, id));

        // Revoke drops the grants but keeps the agent object.
        assert!(dispatch(s, 0, "revoke_grants", &json!({"id": id})).is_ok());
        let r = dispatch(s, 0, "get_agents", &Value::Null).ok().expect("list");
        assert_eq!(r["agents"][0]["grants"], json!([]));

        // Terminate succeeds for a live agent. (Removal of the slot rides on
        // ending its client, which this test agent does not have; that path
        // is covered in protocols/agent/tests.rs.)
        assert!(dispatch(s, 0, "terminate_agent", &json!({"id": id})).is_ok());
    }

    #[test]
    fn unknown_parameters_are_refused() {
        let p = json!({"handle": 1, "wdith": 800});
        assert!(only_keys(&p, &["handle", "width", "height"]).is_err());
        assert!(only_keys(&json!({"handle": 1}), &["handle", "width", "height"]).is_ok());
        // No params at all is an empty object, not an error.
        assert!(only_keys(&Value::Null, &["handle"]).is_ok());
    }

    #[test]
    fn dimensions_must_be_positive_integers() {
        assert!(matches!(opt_dimension(&json!({}), "width"), Ok(None)));
        assert!(matches!(
            opt_dimension(&json!({"width": 800}), "width"),
            Ok(Some(800))
        ));
        for bad in [
            json!({"width": 0}),
            json!({"width": -3}),
            json!({"width": "800"}),
            json!({"width": 12.5}),
        ] {
            assert!(opt_dimension(&bad, "width").is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn chip_rect_validation() {
        let Ok(ok) = parse_chip_rect(&json!({"x": 1.5, "y": -2, "w": 40, "h": 30})) else {
            panic!("valid chip rect refused");
        };
        assert_eq!(
            (ok.loc.x, ok.loc.y, ok.size.w, ok.size.h),
            (1.5, -2.0, 40.0, 30.0)
        );
        for bad in [
            json!({"x": 0, "y": 0, "w": 0, "h": 10}),
            json!({"x": 0, "y": 0, "w": 10, "h": -1}),
            json!({"x": "0", "y": 0, "w": 10, "h": 10}),
            json!({"y": 0, "w": 10, "h": 10}),
            json!(null),
        ] {
            assert!(parse_chip_rect(&bad).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn chip_rect_store_clear_and_cleanup() {
        use crate::shell::focus::state_tests::{client::Client, harness};
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        c.attach(&mut h, &t.surface);
        let w = h.state.space.elements().next().cloned().expect("mapped");
        let id = h.state.ipc.handle_for(&w);
        let rect = json!({"x": 10, "y": 4, "w": 50, "h": 30});
        let call = |h: &mut crate::shell::focus::state_tests::Harness, p: Value| {
            dispatch(&mut h.state, 0, "set_window_chip_rect", &p)
        };

        assert!(call(&mut h, json!({"id": 9999, "output": "test-a", "rect": rect})).is_err());
        assert!(call(&mut h, json!({"id": id, "output": "nope", "rect": rect})).is_err());
        assert!(call(
            &mut h,
            json!({"id": id, "output": "test-a", "rect": {"x": 0, "y": 0, "w": 0, "h": 1}})
        )
        .is_err());
        assert!(h.state.ipc.chip(id).is_none());

        assert!(call(&mut h, json!({"id": id, "output": "test-a", "rect": rect})).is_ok());
        let got = h.state.ipc.chip(id).expect("stored").clone();
        assert_eq!(got.output, "test-a");
        assert_eq!(got.rect.size.w, 50.0);

        assert!(call(&mut h, json!({"id": id, "output": "test-a", "rect": null})).is_ok());
        assert!(h.state.ipc.chip(id).is_none());

        assert!(call(&mut h, json!({"id": id, "output": "test-a", "rect": rect})).is_ok());
        crate::shell::unmap_window(&mut h.state, &w);
        assert!(h.state.ipc.chip(id).is_none());
    }

    #[test]
    fn integer_params_reject_wrong_types() {
        assert!(u64_param(&json!({"output": "1"}), "output").is_err());
        assert!(u64_param(&json!({}), "output").is_err());
        assert!(bool_param(&json!({"enabled": 1}), "enabled").is_err());
    }

    #[test]
    fn overscan_round_trips_through_the_reported_row() {
        // What `set_output` accepts is exactly what a `get_outputs` row
        // reports, edge for edge, so ec-settings can read one and hand it
        // straight back.
        let written = json!({"top": 12, "right": 3, "bottom": 7, "left": 0});
        let parsed = parse_overscan(json!({"overscan": written.clone()}).as_object().expect("object"))
            .ok()
            .flatten()
            .expect("valid, present overscan");
        assert_eq!(overscan_json(parsed), written);

        // Always present, all-zero when nothing is calibrated.
        assert_eq!(
            overscan_json(crate::outputs::overscan::Overscan::default()),
            json!({"top": 0, "right": 0, "bottom": 0, "left": 0})
        );

        // The uniform spelling reports as four equal edges.
        let uniform = parse_overscan(json!({"overscan": 9}).as_object().expect("object"))
            .ok()
            .flatten()
            .expect("valid, present overscan");
        assert_eq!(
            overscan_json(uniform),
            json!({"top": 9, "right": 9, "bottom": 9, "left": 9})
        );

        assert!(matches!(
            parse_overscan(json!({}).as_object().expect("object")),
            Ok(None)
        ));
        assert!(parse_overscan(json!({"overscan": {"middle": 1}}).as_object().expect("object")).is_err());
    }

    #[test]
    fn annotation_title_is_optional_but_must_be_a_string() {
        let base = json!({"x": 0, "y": 0, "w": 100, "h": 50, "text": "body"});
        let (_, _, title, text, pick) = create_params(&base).unwrap_or_else(|_| panic!("no title: rejected"));
        assert_eq!((title, text), ("", "body"));
        assert!(pick.is_empty());

        let mut p = base.clone();
        p["title"] = json!("Q3");
        assert_eq!(
            create_params(&p).unwrap_or_else(|_| panic!("titled: rejected")).2,
            "Q3"
        );

        p["title"] = json!(7);
        assert!(create_params(&p).is_err());
    }

    #[test]
    fn annotation_pick_is_validated_at_the_door() {
        let with_pick = |pick: Value| json!({"x": 0, "y": 0, "w": 100, "h": 50, "text": "t", "pick": pick});
        let ok = create_params(&with_pick(json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": "b2"})))
            .unwrap_or_else(|_| panic!("valid pick: rejected"))
            .4;
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "B2");
        assert_eq!(ok[0].rect, Rectangle::new((1, 2).into(), (3, 4).into()));

        // `null` is absent.
        assert!(create_params(&with_pick(Value::Null))
            .unwrap_or_else(|_| panic!("null pick: rejected"))
            .4
            .is_empty());

        for bad in [
            json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": "ABC"}),
            json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": "\u{e9}"}),
            json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": ""}),
            json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": 1}),
            json!({"x": 1, "y": 2, "w": 3, "h": 4}),
            json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": "A", "colour": "red"}),
            json!({"x": 1, "y": 2, "w": 0, "h": 4, "label": "A"}),
            json!({"x": "1", "y": 2, "w": 3, "h": 4, "label": "A"}),
            json!("A"),
            json!([1, 2, 3, 4]),
        ] {
            assert!(create_params(&with_pick(bad.clone())).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn annotation_anchor_is_optional_and_kind_fails_closed() {
        use crate::render::annotation::AnnotationKind;
        // No rect at all: unanchored, kind defaults to answer.
        let (anchor, kind, ..) =
            create_params(&json!({"text": "t"})).unwrap_or_else(|_| panic!("unanchored: rejected"));
        assert!(anchor.is_none());
        assert_eq!(kind, AnnotationKind::Answer);

        let (_, kind, ..) = create_params(&json!({"text": "t", "kind": "error"}))
            .unwrap_or_else(|_| panic!("error kind: rejected"));
        assert_eq!(kind, AnnotationKind::Error);
        let (_, kind, ..) = create_params(&json!({"text": "t", "kind": "answer"}))
            .unwrap_or_else(|_| panic!("answer kind: rejected"));
        assert_eq!(kind, AnnotationKind::Answer);

        for bad in [
            // Unknown or mistyped kind.
            json!({"text": "t", "kind": "warning"}),
            json!({"text": "t", "kind": "Error"}),
            json!({"text": "t", "kind": null}),
            json!({"text": "t", "kind": 1}),
            // A partial rect is not "no rect".
            json!({"text": "t", "x": 0}),
            json!({"text": "t", "x": 0, "y": 0, "w": 10}),
            // A present rect is still checked.
            json!({"text": "t", "x": 0, "y": 0, "w": 0, "h": 10}),
            json!({"text": "t", "x": 0, "y": 0, "w": 10, "h": -1}),
            // A pick needs an anchor to sit inside.
            json!({"text": "t", "pick": {"x": 1, "y": 2, "w": 3, "h": 4, "label": "A"}}),
        ] {
            assert!(create_params(&bad).is_err(), "{bad} accepted");
        }
        // A null pick without an anchor is simply no pick.
        assert!(create_params(&json!({"text": "t", "pick": null})).is_ok());
    }

    #[test]
    fn annotation_pick_outside_anchor_is_accepted_then_dropped_by_the_store() {
        let p = json!({
            "x": 0, "y": 0, "w": 100, "h": 50, "text": "t",
            "pick": {"x": 500, "y": 500, "w": 10, "h": 10, "label": "A"},
        });
        let (anchor, kind, title, text, pick) =
            create_params(&p).unwrap_or_else(|_| panic!("IPC accepts it: rejected"));
        assert_eq!(pick.len(), 1);
        let mut store = crate::render::annotation::AnnotationStore::default();
        let id = store
            .create_picks(1, anchor, kind, title, text, pick)
            .expect("created");
        let (_, a) = store.iter().find(|(k, _)| **k == id).expect("live");
        assert!(a.picks.is_empty());
    }

    #[test]
    fn annotation_picks_array_is_accepted_in_order() {
        let p = json!({
            "x": 0, "y": 0, "w": 100, "h": 50, "text": "t",
            "picks": [
                {"x": 1, "y": 1, "w": 10, "h": 10, "label": "a"},
                {"x": 20, "y": 1, "w": 10, "h": 10, "label": "c3"},
                {"x": 40, "y": 1, "w": 10, "h": 10, "label": "d"},
            ],
        });
        let picks = create_params(&p).unwrap_or_else(|_| panic!("picks: rejected")).4;
        let labels: Vec<_> = picks.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, ["A", "C3", "D"]);
        assert_eq!(picks[1].rect, Rectangle::new((20, 1).into(), (10, 10).into()));

        // An empty array is no picks; `picks: null` is absent.
        for ok in [json!([]), Value::Null] {
            let p = json!({"x": 0, "y": 0, "w": 100, "h": 50, "text": "t", "picks": ok});
            assert!(create_params(&p)
                .unwrap_or_else(|_| panic!("{ok}: rejected"))
                .4
                .is_empty());
        }
    }

    #[test]
    fn annotation_picks_is_capped_at_sixteen() {
        let pick = |i: i32| json!({"x": i, "y": 0, "w": 1, "h": 1, "label": "A"});
        let with = |n: i32| {
            let items: Vec<Value> = (0..n).map(pick).collect();
            json!({"x": 0, "y": 0, "w": 100, "h": 50, "text": "t", "picks": items})
        };
        assert_eq!(
            create_params(&with(16))
                .unwrap_or_else(|_| panic!("16: rejected"))
                .4
                .len(),
            16
        );
        assert!(create_params(&with(17)).is_err(), "17 picks accepted");
    }

    #[test]
    fn annotation_pick_and_picks_together_are_refused() {
        let both = json!({
            "x": 0, "y": 0, "w": 100, "h": 50, "text": "t",
            "pick": {"x": 1, "y": 1, "w": 3, "h": 3, "label": "A"},
            "picks": [{"x": 1, "y": 1, "w": 3, "h": 3, "label": "B"}],
        });
        assert!(create_params(&both).is_err(), "pick and picks accepted together");
        // A null `pick` is absent, so `picks` alone is fine.
        let null_single = json!({
            "x": 0, "y": 0, "w": 100, "h": 50, "text": "t", "pick": null,
            "picks": [{"x": 1, "y": 1, "w": 3, "h": 3, "label": "B"}],
        });
        assert_eq!(
            create_params(&null_single)
                .unwrap_or_else(|_| panic!("rejected"))
                .4
                .len(),
            1
        );
    }

    #[test]
    fn annotation_picks_entries_are_validated_like_pick() {
        let with = |picks: Value| json!({"x": 0, "y": 0, "w": 100, "h": 50, "text": "t", "picks": picks});
        let good = json!({"x": 1, "y": 2, "w": 3, "h": 4, "label": "B"});
        for bad in [
            // Zero or negative size, a bad label, a missing or extra key, a wrong type.
            json!([good.clone(), {"x": 1, "y": 2, "w": 0, "h": 4, "label": "A"}]),
            json!([good.clone(), {"x": 1, "y": 2, "w": -3, "h": 4, "label": "A"}]),
            json!([good.clone(), {"x": 1, "y": 2, "w": 3, "h": 4, "label": "ABC"}]),
            json!([good.clone(), {"x": 1, "y": 2, "w": 3, "h": 4}]),
            json!([good.clone(), {"x": 1, "y": 2, "w": 3, "h": 4, "label": "A", "colour": "red"}]),
            json!([good.clone(), 7]),
            json!([good.clone(), "A"]),
            json!([good.clone(), null]),
            // Not an array.
            json!(good.clone()),
            json!("B"),
        ] {
            assert!(create_params(&with(bad.clone())).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn annotation_picks_need_an_anchor() {
        let p = json!({"text": "t", "picks": [{"x": 1, "y": 2, "w": 3, "h": 4, "label": "A"}]});
        assert!(create_params(&p).is_err(), "picks without an anchor accepted");
    }

    #[test]
    fn annotation_picks_outside_anchor_are_dropped_one_by_one() {
        // The door admits the shape; the store keeps only the picks wholly
        // inside the anchor (ADR 0054), and keeps the rest.
        let p = json!({
            "x": 0, "y": 0, "w": 100, "h": 50, "text": "t",
            "picks": [
                {"x": 1, "y": 1, "w": 10, "h": 10, "label": "A"},
                {"x": 500, "y": 500, "w": 10, "h": 10, "label": "B"},
                {"x": 20, "y": 20, "w": 10, "h": 10, "label": "C"},
            ],
        });
        let (anchor, kind, title, text, picks) =
            create_params(&p).unwrap_or_else(|_| panic!("IPC accepts it: rejected"));
        let mut store = crate::render::annotation::AnnotationStore::default();
        let id = store
            .create_picks(1, anchor, kind, title, text, picks)
            .expect("created");
        let (_, a) = store.iter().find(|(k, _)| **k == id).expect("live");
        let labels: Vec<_> = a.picks.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, ["A", "C"]);
    }

    #[test]
    fn annotation_update_without_title_clears_it() {
        let anchor = Rectangle::new((0, 0).into(), (100, 50).into());
        let mut store = crate::render::annotation::AnnotationStore::default();
        let id = store
            .create(
                1,
                Some(anchor),
                crate::render::annotation::AnnotationKind::Answer,
                "Old",
                "t",
                None,
            )
            .expect("created");

        let p = json!({"id": id.0, "text": "new"});
        let (raw, title, text) = update_params(&p).unwrap_or_else(|_| panic!("no title is fine: rejected"));
        assert_eq!(title, "");
        let rid = crate::render::annotation::AnnotationId(raw);
        assert!(store.update(1, rid, title, text));
        let (_, a) = store.iter().find(|(k, _)| **k == rid).expect("live");
        assert!(a.title.is_empty());

        assert!(update_params(&json!({"id": 1, "text": "t", "title": false})).is_err());
        assert!(update_params(&json!({"id": 1, "text": "t", "pick": null})).is_err());
    }

    /// Under the lock the focus-relative commands are refused before any
    /// parameter is looked at: they would act on the pre-lock window.
    #[test]
    fn focus_relative_commands_are_refused_under_the_lock() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        s.lock.locked = true;
        for (method, params) in [
            ("focus_window", json!({"handle": 1})),
            ("move_to_workspace", json!({"workspace": 2})),
            ("set_floating", json!({"handle": 1, "floating": true})),
        ] {
            let e = dispatch(s, 0, method, &params).expect_err("refused");
            assert_eq!(e.code, super::super::DENIED, "{method}");
        }
        s.lock.locked = false;
        let e = dispatch(s, 0, "focus_window", &json!({"handle": 1})).expect_err("no such window");
        assert_ne!(
            e.code,
            super::super::DENIED,
            "unlocked, it gets as far as the handle"
        );
    }

    /// ADR 0064: `subscribe` replays the *latest* `config-error` — after a
    /// degraded start and then a failed reload, the reload's, labelled as
    /// such — on every subscribe while it stands, and nothing after a clean
    /// load.
    #[test]
    fn subscribe_replays_the_current_config_error() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        h.state.config.explicit = Some(file.clone());

        let replayed = |state: &mut AbyssState| -> Vec<Value> {
            let (conn, _client) = crate::ipc::test_conn(state);
            crate::ipc::capture::take();
            assert!(subscribe(state, conn, &json!({"events": ["config-error"]})).is_ok());
            crate::ipc::capture::take()
                .into_iter()
                .filter(|(k, _)| k == "config-error")
                .map(|(_, v)| v)
                .collect()
        };

        // A degraded start, as `AbyssState::new` records it.
        std::fs::write(&file, "idle {\n    lock-command 3\n}\n").unwrap();
        let mut start = crate::config::Config::load(Some(&file));
        let _ = start.startup();
        h.state.config_error = Some(crate::config::error_event(&start.errors, true));
        for _ in 0..2 {
            let got = replayed(&mut h.state);
            assert_eq!(got.len(), 1, "{got:?}");
            assert_eq!(got[0]["startup"], true);
        }

        // A failed reload replaces it: new subscribers hear the reload.
        std::fs::write(&file, "general {\n    nope 1\n}\n").unwrap();
        crate::config::watch::reload_now(&mut h.state);
        let got = replayed(&mut h.state);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0]["startup"], false);
        let s = got[0]["summary"].as_str().unwrap_or_default();
        assert!(s.contains("change not applied"), "{s}");
        assert!(s.contains("line 2"), "{s}");

        // A clean load clears it.
        std::fs::write(&file, "general {\n    gaps-in 3\n}\n").unwrap();
        crate::config::watch::reload_now(&mut h.state);
        assert!(h.state.config_error.is_none());
        assert!(replayed(&mut h.state).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADR 0064: a failed reload after a degraded start must not downgrade
    /// the warning. The live config still has auto-lock off, so the reload's
    /// summary (and its replay) keeps leading with it, ahead of "change not
    /// applied". A clean load clears it.
    #[test]
    fn a_failed_reload_keeps_the_degraded_start_lead() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-replay-lead-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");

        // Degraded start with a bad lock-command, as `main` + `AbyssState::new`
        // set it up: the settled config goes live with its refusals.
        std::fs::write(
            &file,
            "idle {\n    lock-timeout-seconds 600\n    lock-command 3\n}\n",
        )
        .unwrap();
        let mut start = crate::config::Config::load(Some(&file));
        assert!(matches!(
            start.startup(),
            crate::config::Startup::Start { ignored: 1 }
        ));
        h.state.config_error = Some(crate::config::error_event(&start.errors, true));
        h.state.config = start;

        // A failed reload of an unrelated key.
        std::fs::write(
            &file,
            "idle {\n    lock-timeout-seconds 600\n    lock-command 3\n}\ngeneral {\n    nope 1\n}\n",
        )
        .unwrap();
        crate::ipc::capture::take();
        crate::config::watch::reload_now(&mut h.state);
        let emitted: Vec<Value> = crate::ipc::capture::take()
            .into_iter()
            .filter(|(k, _)| k == "config-error")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(emitted.len(), 1, "{emitted:?}");

        let (conn, _client) = crate::ipc::test_conn(&mut h.state);
        assert!(subscribe(&mut h.state, conn, &json!({"events": ["config-error"]})).is_ok());
        let replayed: Vec<Value> = crate::ipc::capture::take()
            .into_iter()
            .filter(|(k, _)| k == "config-error")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(replayed.len(), 1, "{replayed:?}");
        for ev in [&emitted[0], &replayed[0]] {
            assert_eq!(ev["startup"], false);
            let s = ev["summary"].as_str().unwrap_or_default();
            assert_eq!(
                s,
                "abyss.kdl: auto-lock is OFF \u{2014} line 3: idle lock-command needs a string argument; \
                 2 problems, change not applied \u{2014} line 3: idle lock-command needs a string argument \
                 (and 1 more; `ec-ctl config validate` lists them)",
            );
        }

        // A clean load clears everything.
        std::fs::write(&file, "general {\n    gaps-in 3\n}\n").unwrap();
        crate::config::watch::reload_now(&mut h.state);
        assert!(h.state.config_error.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
