// SPDX-License-Identifier: AGPL-3.0-only
//! `set_client_strips` (ADR 0079): a browser's add-on helper reports where each
//! of its windows' page area starts, and abyss pins a `translucent-region`
//! strip over the chrome above it.
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
use ec_abyss_render::userdata::ClientStrip;

/// Most entries one call may carry: one per browser window, with room to spare.
const MAX_WINDOWS: usize = 64;
/// The outer size a browser reports and the surface size abyss sees agree to
/// within a pixel (fractional scale rounding).
const SIZE_SLACK: i32 = 1;

/// One window the helper described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    width: i32,
    height: i32,
    content: i32,
}

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

fn parse(params: &Value) -> Result<(f32, Vec<Entry>), RpcError> {
    let bad = |m: &str| RpcError::invalid_params(m);
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    if obj.keys().any(|k| k != "opacity" && k != "windows") {
        return Err(bad("unknown parameter"));
    }
    let opacity = obj
        .get("opacity")
        .and_then(Value::as_f64)
        .filter(|o| o.is_finite())
        .ok_or_else(|| bad("opacity must be a number"))?
        .clamp(0.0, 1.0) as f32;
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
    let mut out = Vec::with_capacity(list.len());
    for w in list {
        out.push(Entry {
            width: dim(w, "width")?,
            height: dim(w, "height")?,
            content: dim(w, "content")?,
        });
    }
    Ok((opacity, out))
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

/// [`set_client_strips`] with the target client pid already resolved.
fn apply(state: &mut AbyssState, conn: u64, parent: i32, params: &Value) -> Result<Value, RpcError> {
    let (opacity, entries) = parse(params)?;
    // A new call replaces the connection's old set.
    clear_for(state, conn);
    let mut applied = 0usize;
    let mut stamped: Vec<Window> = Vec::new();
    for window in state
        .outputs
        .iter()
        .flat_map(|e| e.workspaces.iter())
        .flat_map(|ws| ws.all_windows())
    {
        if window.toplevel().is_none() || super::methods::pid_of(state, &window) != Some(parent) {
            continue;
        }
        let bbox = window.bbox().size;
        let Some(entry) = entries
            .iter()
            .find(|e| size_matches((bbox.w, bbox.h), (e.width, e.height)))
        else {
            continue;
        };
        let top = strip_top(window.geometry().size.h, entry.content);
        stamp(&window, Some((top, opacity)));
        stamped.push(window);
        applied += 1;
    }
    // A window another connection had claimed now answers to this one.
    state.ipc.client_strips.retain(|(_, w)| !stamped.contains(w));
    state
        .ipc
        .client_strips
        .extend(stamped.into_iter().map(|w| (conn, w)));
    if applied > 0 {
        crate::backend::damage_all(state);
    }
    Ok(json!({"ok": true, "applied": applied}))
}

fn stamp(window: &Window, strip: Option<(i32, f32)>) {
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

/// Take every strip `conn` set off again. Damages only if one was showing.
pub(crate) fn clear_for(state: &mut AbyssState, conn: u64) {
    let mine = state.ipc.client_strips.iter().filter(|(c, _)| *c == conn).count();
    if mine == 0 {
        return;
    }
    for (_, w) in state.ipc.client_strips.iter().filter(|(c, _)| *c == conn) {
        stamp(w, None);
    }
    state.ipc.client_strips.retain(|(c, _)| *c != conn);
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
        let (o, e) = parse(&json!({"opacity": 7.0, "windows": [{"width": 1, "height": 2, "content": 1}]}))
            .ok()
            .unwrap();
        assert_eq!((o, e.len()), (1.0, 1));
        for bad in [
            json!({"opacity": 0.5}),
            json!({"windows": []}),
            json!({"opacity": 0.5, "windows": [{"width": -1, "height": 2, "content": 1}]}),
            json!({"opacity": 0.5, "windows": [{"width": 1, "height": 2}]}),
            json!({"opacity": 0.5, "windows": [], "extra": 1}),
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
        let p = json!({"opacity": 0.6, "windows": [{"width": size, "height": size, "content": content}]});
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
        assert_eq!(strip_of(&w), Some((30, 0.6)));
        // A new call replaces the old set: this one matches nothing.
        assert_eq!(call(&mut h, 1, me, 300, 70)["applied"], 0);
        assert_eq!(strip_of(&w), None);
    }

    #[test]
    fn strip_height_is_clamped() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        call(&mut h, 1, me, 100, 0);
        assert_eq!(strip_of(&w), Some((50, 0.6)));
        // A page taller than the window leaves no strip.
        call(&mut h, 1, me, 100, 500);
        assert_eq!(strip_of(&w), None);
    }

    #[test]
    fn a_rule_beats_a_client_strip() {
        let mut h = harness();
        let (_c, w, me) = mapped(&mut h);
        call(&mut h, 1, me, 100, 70);
        assert_eq!(strip_of(&w), Some((30, 0.6)));
        w.user_data()
            .insert_if_missing(|| RuleStrip(Cell::new(Some((12, 0.3)))));
        assert_eq!(strip_of(&w), Some((12, 0.3)));
        // The rule going away hands the strip back to the client's.
        w.user_data().get::<RuleStrip>().unwrap().0.set(None);
        assert_eq!(strip_of(&w), Some((30, 0.6)));
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
        assert_eq!(strip_of(&w), Some((40, 0.6)));
        clear_for(&mut h.state, 1);
        assert_eq!(strip_of(&w), Some((40, 0.6)));
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
