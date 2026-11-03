// SPDX-License-Identifier: AGPL-3.0-only
//! `set_client_strips` (ADR 0079): a browser's add-on helper reports its
//! windows and the key colours its glass surfaces are painted in, and abyss
//! pins a tint-keyed `translucent-region` strip over the chrome above the page
//! (or over the whole window while `body` is set). Only key-coloured pixels
//! turn translucent.
//!
//! The helper reports a resize before the browser commits the resized buffer,
//! so each connection's latest request is kept and re-matched whenever a window
//! of that client commits a new size ([`on_window_commit`]).
//!
//! Authority is narrower than the gate's owner-uid check. The caller's parent
//! process (read from `/proc/<pid>/stat`) names the only client whose windows
//! it may touch, so a helper started by Firefox reaches Firefox's toplevels and
//! nothing else. A strip lives and dies with the connection that set it, and a
//! user `translucent-region` rule beats it (`ec_abyss_render::userdata::strip_of`).

use std::cell::Cell;

use serde_json::{json, Value};
use smithay::desktop::Window;

use super::RpcError;
use crate::state::AbyssState;
use ec_abyss_render::userdata::{ClientStrip, Keys, Strip, StripFill};

/// Most entries one call may carry: one per browser window, with room to spare.
const MAX_WINDOWS: usize = 64;
/// Most key colours one call may declare.
const MAX_KEYS: usize = 4;
/// The outer size a browser reports and the surface size abyss sees agree to
/// within a pixel (fractional scale rounding).
const SIZE_SLACK: i32 = 1;

/// The parent of `pid`, from its `/proc/<pid>/stat`.
pub(crate) fn parent_pid(pid: i32) -> Option<i32> {
    parse_ppid(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Field 4 of a stat line. `comm` (field 2) sits in parentheses and may hold
/// spaces and parentheses itself, so count from the last `)`.
fn parse_ppid(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_ascii_whitespace()
        .nth(1)?
        .parse()
        .ok()
        .filter(|p| *p > 0)
}

/// Height of the strip: the geometry above the page area, kept within the top
/// half so a bad report can never glass most of a window.
pub(crate) fn strip_top(geometry_h: i32, content: i32) -> i32 {
    (geometry_h - content).clamp(0, geometry_h / 2)
}

fn size_matches(have: (i32, i32), want: (i32, i32)) -> bool {
    (have.0 - want.0).abs() <= SIZE_SLACK && (have.1 - want.1).abs() <= SIZE_SLACK
}

/// One window the helper described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    width: i32,
    height: i32,
    content: i32,
    /// Key the whole window, not just the strip.
    body: bool,
}

/// A connection's `set_client_strips` request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Request {
    keys: Keys,
    entries: Vec<Entry>,
}

/// The last `(bbox w, bbox h, geometry h)` a window's commit was matched at.
struct SeenSize(Cell<(i32, i32, i32)>);

/// `#rrggbb`, exactly.
fn parse_color(v: &str) -> Option<[u8; 3]> {
    let h = v.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn parse(params: &Value) -> Result<Request, RpcError> {
    let bad = |m: &str| RpcError::invalid_params(m);
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    if obj.keys().any(|k| k != "keys" && k != "windows") {
        return Err(bad("unknown parameter"));
    }
    let key_list = obj
        .get("keys")
        .and_then(Value::as_array)
        .filter(|k| (1..=MAX_KEYS).contains(&k.len()))
        .ok_or_else(|| bad("keys must be an array of 1 to 4 entries"))?;
    let mut keys = Keys::one([0; 3], 1.0);
    keys.n = key_list.len() as u8;
    for (i, k) in key_list.iter().enumerate() {
        keys.rgb[i] = k
            .get("color")
            .and_then(Value::as_str)
            .and_then(parse_color)
            .ok_or_else(|| bad("key color must be \"#rrggbb\""))?;
        keys.opacity[i] = k
            .get("opacity")
            .and_then(Value::as_f64)
            .filter(|o| (0.0..=1.0).contains(o))
            .ok_or_else(|| bad("key opacity must be a number in 0..=1"))? as f32;
        keys.everywhere[i] = match k.get("everywhere") {
            None => false,
            Some(b) => b
                .as_bool()
                .ok_or_else(|| bad("key everywhere must be a boolean"))?,
        };
    }
    let list = obj
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("windows must be an array"))?;
    if list.len() > MAX_WINDOWS {
        return Err(bad("too many windows"));
    }
    let dim = |w: &Value, k: &str| {
        w.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| bad("width, height and content must be non-negative integers"))
    };
    let mut entries = Vec::with_capacity(list.len());
    for w in list {
        let body = match w.get("body") {
            None => false,
            Some(b) => b.as_bool().ok_or_else(|| bad("body must be a boolean"))?,
        };
        entries.push(Entry {
            width: dim(w, "width")?,
            height: dim(w, "height")?,
            content: dim(w, "content")?,
            body,
        });
    }
    Ok(Request { keys, entries })
}

/// The method. `parent` is the caller's parent pid; `None` (unreadable stat)
/// reaches nothing.
pub(crate) fn set_client_strips(
    state: &mut AbyssState,
    conn: u64,
    params: &Value,
) -> Result<Value, RpcError> {
    let parent = state
        .ipc
        .peer_pid(conn)
        .and_then(parent_pid)
        .ok_or_else(|| RpcError::denied("the caller has no readable parent process"))?;
    apply(state, conn, parent, params)
}

/// The strip the latest request that matches a window of client `pid` gives
/// it, with the connection that asked. Allocation-free.
fn wanted(
    requests: &[(u64, i32, Request)],
    pid: i32,
    bbox: (i32, i32),
    geometry_h: i32,
) -> Option<(u64, Strip)> {
    requests
        .iter()
        .rev()
        .filter(|(_, p, _)| *p == pid)
        .find_map(|(conn, _, req)| {
            let e = req
                .entries
                .iter()
                .find(|e| size_matches(bbox, (e.width, e.height)))?;
            let top = if e.body {
                Strip::WHOLE
            } else {
                strip_top(geometry_h, e.content)
            };
            Some((
                *conn,
                Strip {
                    top,
                    fill: StripFill::Keyed(req.keys),
                },
            ))
        })
}

/// Bring one window's strip in line with the stored requests. Returns whether
/// anything changed. Allocates only when it does. With `keep_stale`, a window no
/// stored request matches keeps its current stamp instead of losing it.
fn settle(state: &mut AbyssState, window: &Window, pid: i32, keep_stale: bool) -> bool {
    let bbox = window.bbox().size;
    let want = wanted(
        &state.ipc.client_requests,
        pid,
        (bbox.w, bbox.h),
        window.geometry().size.h,
    );
    let have = state
        .ipc
        .client_strips
        .iter()
        .find(|(_, w)| w == window)
        .map(|(c, _)| *c)
        .zip(window.user_data().get::<ClientStrip>().and_then(|s| s.0.get()));
    if want == have || (keep_stale && want.is_none()) {
        return false;
    }
    state.ipc.client_strips.retain(|(_, w)| w != window);
    stamp(window, want.map(|(_, s)| s));
    if let Some((conn, _)) = want {
        state.ipc.client_strips.push((conn, window.clone()));
    }
    true
}

/// A toplevel committed. If its client has a stored request and the window's
/// size changed since last looked at, match it again. A commit that matches
/// nothing keeps the stamp it has: a browser commits its resized buffer before
/// the helper's report of the new size arrives, and clearing then would flash
/// the window opaque. The strip height and keys do not depend on the size, so
/// a stale stamp is invisible. It goes when a request matches, when a request
/// no longer includes the window, or when the connection leaves.
pub(crate) fn on_window_commit(state: &mut AbyssState, window: &Window) {
    if state.ipc.client_requests.is_empty() || window.toplevel().is_none() {
        return;
    }
    let (bbox, geo) = (window.bbox().size, window.geometry().size);
    let size = (bbox.w, bbox.h, geo.h);
    match window.user_data().get::<SeenSize>() {
        Some(seen) if seen.0.get() == size => return,
        Some(seen) => seen.0.set(size),
        None => {
            window.user_data().insert_if_missing(|| SeenSize(Cell::new(size)));
        }
    }
    let Some(pid) = super::methods::pid_of(state, window) else {
        return;
    };
    if state.ipc.client_requests.iter().all(|(_, p, _)| *p != pid) {
        return;
    }
    if settle(state, window, pid, true) {
        crate::backend::damage_all(state);
    }
}

/// [`set_client_strips`] with the target client pid already resolved.
fn apply(state: &mut AbyssState, conn: u64, parent: i32, params: &Value) -> Result<Value, RpcError> {
    let request = parse(params)?;
    // A new call replaces the connection's old request.
    clear_for(state, conn);
    state.ipc.client_requests.push((conn, parent, request));
    let windows: Vec<Window> = state
        .outputs
        .iter()
        .flat_map(|e| e.workspaces.iter())
        .flat_map(|ws| ws.all_windows())
        .filter(|w| w.toplevel().is_some() && super::methods::pid_of(state, w) == Some(parent))
        .collect();
    let mut applied = 0usize;
    for window in &windows {
        settle(state, window, parent, false);
        if state
            .ipc
            .client_strips
            .iter()
            .any(|(c, w)| *c == conn && w == window)
        {
            applied += 1;
        }
    }
    if applied > 0 {
        crate::backend::damage_all(state);
    }
    Ok(json!({"ok": true, "applied": applied}))
}

fn stamp(window: &Window, strip: Option<Strip>) {
    match window.user_data().get::<ClientStrip>() {
        Some(slot) => slot.0.set(strip),
        None if strip.is_some() => {
            window
                .user_data()
                .insert_if_missing(|| ClientStrip(Cell::new(strip)));
        }
        None => {}
    }
}

/// Drop `conn`'s request and take every strip it set off again, handing a
/// window to another connection's matching request if there is one. Damages
/// only if something was showing.
pub(crate) fn clear_for(state: &mut AbyssState, conn: u64) {
    state.ipc.client_requests.retain(|(c, ..)| *c != conn);
    let mine: Vec<Window> = state
        .ipc
        .client_strips
        .iter()
        .filter(|(c, _)| *c == conn)
        .map(|(_, w)| w.clone())
        .collect();
    if mine.is_empty() {
        return;
    }
    state.ipc.client_strips.retain(|(c, _)| *c != conn);
    for w in &mine {
        stamp(w, None);
        if let Some(pid) = super::methods::pid_of(state, w) {
            settle(state, w, pid, false);
        }
    }
    crate::backend::damage_all(state);
}

/// The window unmapped: whoever set its strip no longer owns anything on it.
pub(crate) fn forget_window(state: &mut AbyssState, window: &Window) {
    if state.ipc.client_strips.iter().any(|(_, w)| w == window) {
        stamp(window, None);
        state.ipc.client_strips.retain(|(_, w)| w != window);
        crate::backend::damage_all(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::focus::state_tests::{client::Client, harness, Harness};
    use ec_abyss_render::userdata::{strip_of, RuleStrip};

    const KEY: [u8; 3] = [0x11, 0x22, 0x33];

    fn keyed(top: i32) -> Option<Strip> {
        Some(Strip {
            top,
            fill: StripFill::Keyed(Keys::one(KEY, 0.6)),
        })
    }

    #[test]
    fn ppid_parses_past_a_hostile_comm() {
        assert_eq!(parse_ppid("123 (firefox) S 77 123 123 0"), Some(77));
        assert_eq!(parse_ppid("123 (a) b (c) S 88 1 1"), Some(88));
        assert_eq!(parse_ppid("123 (x) S 0 1 1"), None);
        assert_eq!(parse_ppid("garbage"), None);
        assert_eq!(
            parent_pid(std::process::id() as i32),
            Some(unsafe { libc::getppid() })
        );
    }

    #[test]
    fn top_is_geometry_minus_content_clamped_to_the_top_half() {
        assert_eq!(strip_top(800, 720), 80);
        assert_eq!(strip_top(800, 800), 0);
        assert_eq!(strip_top(800, 900), 0);
        assert_eq!(strip_top(800, 100), 400);
        assert_eq!(strip_top(800, 0), 400);
    }

    #[test]
    fn sizes_match_within_a_pixel() {
        assert!(size_matches((100, 100), (100, 100)));
        assert!(size_matches((100, 100), (101, 99)));
        assert!(!size_matches((100, 100), (102, 100)));
        assert!(!size_matches((100, 100), (100, 98)));
    }

    #[test]
    fn parameters_are_checked() {
        let ok = parse(&json!({
            "keys": [{"color": "#1B2c3D", "opacity": 0.35}, {"color": "#000000", "opacity": 0}],
            "windows": [{"width": 1, "height": 2, "content": 1}, {"width": 3, "height": 4, "content": 0, "body": true}]
        }))
        .ok()
        .unwrap();
        assert_eq!(ok.keys.n, 2);
        assert_eq!(ok.keys.rgb[0], [0x1b, 0x2c, 0x3d]);
        assert_eq!(ok.keys.opacity[1], 0.0);
        assert_eq!(ok.keys.everywhere, [false; 4]);
        let ev = parse(
            &json!({"keys": [{"color": "#000001", "opacity": 0.5, "everywhere": true}], "windows": []}),
        )
        .ok()
        .unwrap();
        assert!(ev.keys.everywhere[0] && ev.keys.any_everywhere());
        assert_eq!(ok.entries.len(), 2);
        assert!(!ok.entries[0].body && ok.entries[1].body);
        let k = json!([{"color": "#112233", "opacity": 0.5}]);
        let w = json!([{"width": 1, "height": 2, "content": 1}]);
        let five = json!((0..5)
            .map(|_| json!({"color": "#112233", "opacity": 0.5}))
            .collect::<Vec<_>>());
        for bad in [
            // The old v1 shape.
            json!({"opacity": 0.5, "windows": w}),
            json!({"windows": w}),
            json!({"keys": k}),
            json!({"keys": [], "windows": w}),
            json!({"keys": five, "windows": w}),
            json!({"keys": [{"color": "112233", "opacity": 0.5}], "windows": w}),
            json!({"keys": [{"color": "#11223", "opacity": 0.5}], "windows": w}),
            json!({"keys": [{"color": "#11223g", "opacity": 0.5}], "windows": w}),
            json!({"keys": [{"color": "#112233"}], "windows": w}),
            json!({"keys": [{"color": "#112233", "opacity": 1.5}], "windows": w}),
            json!({"keys": [{"color": "#112233", "opacity": -0.1}], "windows": w}),
            json!({"keys": [{"color": "#112233", "opacity": "x"}], "windows": w}),
            json!({"keys": k, "windows": [{"width": -1, "height": 2, "content": 1}]}),
            json!({"keys": k, "windows": [{"width": 1, "height": 2}]}),
            json!({"keys": k, "windows": [{"width": 1, "height": 2, "content": 1, "body": 1}]}),
            json!({"keys": [{"color": "#112233", "opacity": 0.5, "everywhere": 1}], "windows": w}),
            json!({"keys": k, "windows": w, "extra": 1}),
            json!(null),
        ] {
            assert!(parse(&bad).is_err(), "{bad} accepted");
        }
    }

    fn mapped(h: &mut Harness) -> (Client, Window, i32) {
        let mut c = Client::connect(h);
        let t = c.create_toplevel(h);
        c.commit(h, &t.surface);
        c.attach(h, &t.surface);
        let w = h.state.space.elements().next().cloned().expect("mapped");
        // The harness client lives in this process.
        (c, w, std::process::id() as i32)
    }

    fn call(h: &mut Harness, conn: u64, parent: i32, size: i64, content: i64) -> Value {
        call_body(h, conn, parent, size, content, false)
    }

    fn call_body(h: &mut Harness, conn: u64, parent: i32, size: i64, content: i64, body: bool) -> Value {
        let p = json!({
            "keys": [{"color": "#112233", "opacity": 0.6}],
            "windows": [{"width": size, "height": size, "content": content, "body": body}]
        });
        apply(&mut h.state, conn, parent, &p).ok().expect("accepted")
    }

    #[test]
    fn only_the_parents_windows_are_touched_and_size_gates_the_match() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        // Another pid's windows are untouched, even at an exact size match.
        assert_eq!(call(&mut h, 1, me + 1, 100, 70)["applied"], 0);
        assert_eq!(strip_of(&w), None);
        // A size off by two is not this window.
        assert_eq!(call(&mut h, 1, me, 102, 70)["applied"], 0);
        assert_eq!(strip_of(&w), None);
        // Off by one is; the 100 px geometry less 70 px of page is a 30 px strip.
        assert_eq!(call(&mut h, 1, me, 101, 70)["applied"], 1);
        assert_eq!(strip_of(&w), keyed(30));
        // A new call replaces the old set: this one matches nothing.
        assert_eq!(call(&mut h, 1, me, 300, 70)["applied"], 0);
        assert_eq!(strip_of(&w), None);
    }

    #[test]
    fn strip_height_is_clamped() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        call(&mut h, 1, me, 100, 0);
        assert_eq!(strip_of(&w), keyed(50));
        // A page taller than the window leaves no strip.
        call(&mut h, 1, me, 100, 500);
        assert_eq!(strip_of(&w), None);
    }

    #[test]
    fn a_rule_beats_a_client_strip() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        call(&mut h, 1, me, 100, 70);
        assert_eq!(strip_of(&w), keyed(30));
        w.user_data()
            .insert_if_missing(|| RuleStrip(Cell::new(Some(Strip::fade(12, 0.3)))));
        assert_eq!(strip_of(&w), Some(Strip::fade(12, 0.3)));
        // The rule going away hands the strip back to the client's.
        w.user_data().get::<RuleStrip>().unwrap().0.set(None);
        assert_eq!(strip_of(&w), keyed(30));
    }

    #[test]
    fn the_strip_goes_with_its_connection_and_with_the_window() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        call(&mut h, 1, me, 100, 70);
        // Another connection disconnecting leaves it alone.
        clear_for(&mut h.state, 2);
        assert!(strip_of(&w).is_some());
        clear_for(&mut h.state, 1);
        assert_eq!(strip_of(&w), None);
        assert!(h.state.ipc.client_strips.is_empty());

        call(&mut h, 1, me, 100, 70);
        forget_window(&mut h.state, &w);
        assert_eq!(strip_of(&w), None);
        assert!(h.state.ipc.client_strips.is_empty());
    }

    #[test]
    fn a_second_connection_takes_over_and_the_first_leaving_does_not_clear_it() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        call(&mut h, 1, me, 100, 70);
        call(&mut h, 2, me, 100, 60);
        assert_eq!(strip_of(&w), keyed(40));
        clear_for(&mut h.state, 1);
        assert_eq!(strip_of(&w), keyed(40));
    }

    #[test]
    fn body_keys_the_whole_window() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        assert_eq!(call_body(&mut h, 1, me, 100, 70, true)["applied"], 1);
        assert_eq!(strip_of(&w).map(|s| s.top), Some(Strip::WHOLE));
        call_body(&mut h, 1, me, 100, 70, false);
        assert_eq!(strip_of(&w).map(|s| s.top), Some(30));
    }

    #[test]
    fn a_request_before_the_resize_commit_is_matched_and_a_stale_stamp_survives_a_commit() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        c.attach(&mut h, &t.surface);
        let w = h.state.space.elements().next().cloned().expect("mapped");
        let me = std::process::id() as i32;
        // The helper reports the new 160 px size before the browser commits it:
        // nothing matches the 100 px buffer yet.
        assert_eq!(call(&mut h, 1, me, 160, 120)["applied"], 0);
        assert_eq!(strip_of(&w), None);
        // The resized buffer lands: the stored request matches, no resend.
        c.attach_sized(&mut h, &t.surface, 160, 160);
        assert_eq!(strip_of(&w), keyed(40));
        // Resized away from every reported size: the stamp stays, so the
        // window does not flash opaque while the next report is in flight.
        c.attach_sized(&mut h, &t.surface, 100, 100);
        assert_eq!(strip_of(&w), keyed(40));
        // And back.
        c.attach_sized(&mut h, &t.surface, 160, 160);
        assert_eq!(strip_of(&w), keyed(40));
        // A commit that matches nothing, then the matching report: the stamp
        // persists, then updates to the new request's strip.
        c.attach_sized(&mut h, &t.surface, 200, 200);
        assert_eq!(strip_of(&w), keyed(40));
        assert_eq!(call(&mut h, 1, me, 200, 150)["applied"], 1);
        assert_eq!(strip_of(&w), keyed(50));
        // A request that no longer includes the window clears it.
        assert_eq!(call(&mut h, 1, me, 300, 150)["applied"], 0);
        assert_eq!(strip_of(&w), None);
        assert_eq!(call(&mut h, 1, me, 200, 150)["applied"], 1);
        // The connection leaving drops the request with its strips.
        clear_for(&mut h.state, 1);
        assert!(h.state.ipc.client_requests.is_empty());
        c.attach_sized(&mut h, &t.surface, 100, 100);
        c.attach_sized(&mut h, &t.surface, 160, 160);
        assert_eq!(strip_of(&w), None);
    }

    #[test]
    fn commits_without_a_request_or_a_size_change_do_nothing() {
        let mut h = harness();
        let (_c, w, _me) = mapped(&mut h);
        // No requests: the commit path exits before touching the window.
        on_window_commit(&mut h.state, &w);
        assert!(w.user_data().get::<SeenSize>().is_none());
    }

    #[test]
    fn the_hook_gates_the_method() {
        use ec_abyss_wire::{
            gate::{check, check_hook, Decision, Peer},
            hooks::{Hook, HookSet},
        };
        let owner = Peer {
            uid: 1000,
            pid: 5,
            comm: None,
        };
        let cfg = ec_abyss_config::Config::default();
        let outer = check(&owner, 1000, &cfg, "set_client_strips");
        assert_eq!(outer, Decision::Allow);
        let p = json!({});
        assert!(matches!(
            check_hook(outer, HookSet::default(), "set_client_strips", &p),
            Decision::Deny("add-on hook `client-strips` is off")
        ));
        let mut on = HookSet::default();
        on.insert(Hook::ClientStrips);
        assert_eq!(check_hook(outer, on, "set_client_strips", &p), Decision::Allow);
    }
}
