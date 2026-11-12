// SPDX-License-Identifier: AGPL-3.0-only
//! The `capture` control-socket event (ADR 0106): `{"active": bool}`, sent
//! when a human capture client (`zwlr_screencopy_v1`,
//! `ext_image_copy_capture_v1`) starts or stops being served. No consumer
//! name, no counts: a toast daemon only needs to know whether to hold.
//!
//! "Active" is the capture indicator's own answer (`AbyssState::capture_active`,
//! a frame served in the last [`INDICATOR_LINGER`]) or an open
//! `ext_image_copy_capture` session, which covers a stream that is quiet
//! because the screen is. The handlers call [`sync`] when they start serving
//! or drop a session; while active, a calloop timer re-checks so the end of a
//! share (the last frame ageing out) is announced too. Single-threaded, no
//! new thread or lock.
//!
//! [`INDICATOR_LINGER`]: crate::render::capture::INDICATOR_LINGER

use std::time::Duration;

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::state::AbyssState;

/// How often an active share is re-checked for having ended.
pub const POLL: Duration = Duration::from_millis(500);

/// What was last announced, and whether the re-check timer is armed.
#[derive(Debug, Default)]
pub struct Announced {
    active: bool,
    polling: bool,
}

/// Is a human capture running right now?
pub fn is_active(state: &AbyssState) -> bool {
    state.capture_active() || state.image_copy.has_sessions()
}

/// The last announced value, for replay to a client that has just subscribed.
pub fn announced(state: &AbyssState) -> bool {
    state.image_copy.announced.active
}

/// Announce a change of [`is_active`], if there was one, and keep the
/// re-check timer running while active.
pub fn sync(state: &mut AbyssState) {
    let now = is_active(state);
    if now != state.image_copy.announced.active {
        state.image_copy.announced.active = now;
        tracing::info!(active = now, "capture state changed");
        crate::ipc::emit(state, "capture", serde_json::json!({"active": now}));
    }
    if now && !state.image_copy.announced.polling {
        state.image_copy.announced.polling = true;
        let armed = state
            .loop_handle
            .insert_source(Timer::from_duration(POLL), |_, _, st| sync_inner(st));
        if armed.is_err() {
            state.image_copy.announced.polling = false;
        }
    }
}

fn sync_inner(st: &mut AbyssState) -> TimeoutAction {
    // `sync` would arm a second timer while this one is still registered.
    st.image_copy.announced.polling = true;
    let now = is_active(st);
    if now != st.image_copy.announced.active {
        st.image_copy.announced.active = now;
        tracing::info!(active = now, "capture state changed");
        crate::ipc::emit(st, "capture", serde_json::json!({"active": now}));
    }
    if now {
        TimeoutAction::ToDuration(POLL)
    } else {
        st.image_copy.announced.polling = false;
        TimeoutAction::Drop
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_event_name_is_subscribable() {
        assert!(ec_abyss_wire::events::EVENTS.contains(&"capture"));
    }
}
