// SPDX-License-Identifier: AGPL-3.0-only
//! Control-socket method handlers (COMP-13 §2.1).
//!
//! Every function here runs only after [`super::gate::check`] returned
//! `Allow`, on the compositor thread, between two frames. They may touch
//! `HeliosState` freely; they must not block.
//!
//! What is deliberately absent is as much of the design as what is present:
//! no `get_tree`, no capture, no grant manipulation, and no window *content*
//! of any kind (COMP-13 §2).

use serde_json::{json, Map, Value};
use smithay::{desktop::Window, utils::IsAlive, wayland::compositor::with_states};

use super::RpcError;
use crate::state::HeliosState;

type Reply = Result<Value, RpcError>;

pub fn dispatch(state: &mut HeliosState, conn: u64, method: &str, params: &Value) -> Reply {
    match method {
        // Queries.
        "get_workspaces" => get_workspaces(state),
        "get_outputs" => get_outputs(state, params),
        "get_windows" => get_windows(state),
        "get_focused" => get_focused(state),
        "get_metrics" => get_metrics(state),
        "dump_state" => dump_state(state),
        // Subscription.
        "subscribe" => subscribe(state, conn, params),
        "unsubscribe" => unsubscribe(state, conn),
        // Commands.
        "focus_window" => focus_window(state, params),
        "close_window" => close_window(state, params),
        "move_to_workspace" => move_to_workspace(state, params),
        "set_floating" => set_floating(state, params),
        "switch_workspace" => switch_workspace(state, params),
        "reload_config" => reload_config(state),
        // Unreachable: the gate rejects anything not in the table and
        // `handle_line` rejects anything the table marks unimplemented.
        other => Err(RpcError::not_implemented(other)),
    }
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

fn window_param(state: &HeliosState, params: &Value) -> Result<Window, RpcError> {
    let handle = u64_param(params, "handle")?;
    state
        .ipc
        .window_for(handle)
        .filter(IsAlive::alive)
        .ok_or_else(|| RpcError::invalid_params("no such window handle"))
}

/// `app_id` and `title` for a toplevel. Read here and returned to the owner;
/// never written to the journal (COMP-13 §2, ADR 0028).
fn identity_of(window: &Window) -> (Option<String>, Option<String>) {
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

/// Where a window lives, as `(output id, 1-based workspace)`.
fn location_of(state: &HeliosState, window: &Window) -> Option<(u64, usize)> {
    for entry in state.outputs.iter() {
        for (i, ws) in entry.workspaces.iter().enumerate() {
            if ws.windows().iter().any(|w| w == window) {
                return Some((entry.id, i + 1));
            }
        }
    }
    None
}

fn is_floating(state: &HeliosState, window: &Window) -> bool {
    state.outputs.iter().any(|e| {
        e.workspaces
            .iter()
            .any(|ws| ws.floating.iter().any(|f| &f.window == window))
    })
}

/// Focus `window` so the focus-relative shell operations act on it. The
/// shell's command surface is focus-based; the socket addresses windows by
/// handle, and this is the bridge.
fn retarget(state: &mut HeliosState, window: &Window) {
    if state.focus.as_ref() != Some(window) {
        crate::shell::focus_window(state, window);
    }
}

// ---------------------------------------------------------------- queries

fn get_workspaces(state: &mut HeliosState) -> Reply {
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

fn get_outputs(state: &mut HeliosState, params: &Value) -> Reply {
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
            "position": position.map(|p| json!({"x": p.x, "y": p.y})),
            "workspaces": entry.workspaces.len(),
            "active_workspace": entry.active + 1,
        }));
    }
    Ok(Value::Array(out))
}

fn get_windows(state: &mut HeliosState) -> Reply {
    state.ipc.gc();
    let windows: Vec<Window> = state.space.elements().cloned().collect();
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
            "output": output,
            "workspace": workspace,
            "floating": is_floating(state, &w),
            "focused": state.focus.as_ref() == Some(&w),
            // The default class for anything not explicitly raised
            // (root invariant: default is `private`). `secret` and
            // `no-agent` come from `windowrule`, which the config layer does
            // not parse yet, so this is the floor and never a claim of less.
            "trust": if sensitive { "secret" } else { "private" },
            "no_agent": sensitive,
        }));
    }
    Ok(Value::Array(out))
}

fn get_focused(state: &mut HeliosState) -> Reply {
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

fn get_metrics(state: &mut HeliosState) -> Reply {
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

fn dump_state(state: &mut HeliosState) -> Reply {
    Ok(json!({
        "outputs": get_outputs(state, &json!({"all": true}))?,
        "workspaces": get_workspaces(state)?,
        "windows": get_windows(state)?,
        "focused": get_focused(state)?,
        "metrics": get_metrics(state)?,
        "config": {
            "sources": state.config.sources.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "scripted_input": state.config.misc.scripted_input,
        },
    }))
}

// ----------------------------------------------------------- subscription

/// Event kinds a client may ask for (COMP-13 §2.1). An unknown kind is
/// rejected rather than silently accepted: a bar that thinks it is subscribed
/// and never hears anything is the worst outcome.
const EVENTS: &[&str] = &[
    "workspace",
    "window",
    "focus",
    "output",
    "agent-activity",
    "config-error",
];

fn subscribe(state: &mut HeliosState, conn: u64, params: &Value) -> Reply {
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
    Ok(json!({"subscribed": wanted}))
}

fn unsubscribe(state: &mut HeliosState, conn: u64) -> Reply {
    if let Some(c) = state.ipc.conn_mut(conn) {
        c.subs.clear();
    }
    Ok(json!({"subscribed": []}))
}

// --------------------------------------------------------------- commands

fn focus_window(state: &mut HeliosState, params: &Value) -> Reply {
    let w = window_param(state, params)?;
    crate::shell::focus_window(state, &w);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

fn close_window(state: &mut HeliosState, params: &Value) -> Reply {
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

fn move_to_workspace(state: &mut HeliosState, params: &Value) -> Reply {
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

fn set_floating(state: &mut HeliosState, params: &Value) -> Reply {
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

fn switch_workspace(state: &mut HeliosState, params: &Value) -> Reply {
    let idx = u64_param(params, "workspace")? as usize;
    crate::shell::switch_workspace(state, idx);
    crate::backend::damage_all(state);
    Ok(json!({"ok": true}))
}

fn reload_config(state: &mut HeliosState) -> Reply {
    crate::config::watch::reload_now(state);
    Ok(json!({
        "ok": true,
        "sources": state.config.sources.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    }))
}
