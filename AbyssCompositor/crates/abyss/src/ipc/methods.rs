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
use smithay::{desktop::Window, utils::IsAlive, wayland::compositor::with_states};

use super::RpcError;
use crate::state::AbyssState;

type Reply = Result<Value, RpcError>;

pub fn dispatch(state: &mut AbyssState, conn: u64, method: &str, params: &Value) -> Reply {
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
        "resize" => resize(state, params),
        "move_workspace_to_output" => move_workspace_to_output(state, params),
        "set_output" => set_output(state, params),
        "calibrate_output" => calibrate_output(state, params),
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
pub(crate) fn identity_of(window: &Window) -> (Option<String>, Option<String>) {
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
fn location_of(state: &AbyssState, window: &Window) -> Option<(u64, usize)> {
    for entry in state.outputs.iter() {
        for (i, ws) in entry.workspaces.iter().enumerate() {
            if ws.windows().iter().any(|w| w == window) {
                return Some((entry.id, i + 1));
            }
        }
    }
    None
}

fn is_floating(state: &AbyssState, window: &Window) -> bool {
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
            "position": position.map(|p| json!({"x": p.x, "y": p.y})),
            "workspaces": entry.workspaces.len(),
            "active_workspace": entry.active + 1,
        }));
    }
    Ok(Value::Array(out))
}

fn get_windows(state: &mut AbyssState) -> Reply {
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
    Ok(json!({"subscribed": wanted}))
}

fn unsubscribe(state: &mut AbyssState, conn: u64) -> Reply {
    if let Some(c) = state.ipc.conn_mut(conn) {
        c.subs.clear();
    }
    Ok(json!({"subscribed": []}))
}

// --------------------------------------------------------------- commands

fn focus_window(state: &mut AbyssState, params: &Value) -> Reply {
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

fn move_to_workspace(state: &mut AbyssState, params: &Value) -> Reply {
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
    // `overscan` is either a single number for all four edges or an object with
    // any subset of the edges; the omitted ones stay at zero, matching the
    // config spelling exactly so the two front ends cannot drift.
    let overscan = match obj.get("overscan") {
        None => None,
        Some(Value::Number(n)) => {
            let px = edge_px(n.as_i64())?;
            Some(crate::outputs::overscan::Overscan::uniform(px))
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
            Some(v)
        }
        Some(_) => {
            return Err(RpcError::invalid_params(
                "overscan must be a number or {top, bottom, left, right}",
            ))
        }
    };

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn integer_params_reject_wrong_types() {
        assert!(u64_param(&json!({"output": "1"}), "output").is_err());
        assert!(u64_param(&json!({}), "output").is_err());
        assert!(bool_param(&json!({"enabled": 1}), "enabled").is_err());
    }
}
