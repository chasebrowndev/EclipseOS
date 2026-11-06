// SPDX-License-Identifier: AGPL-3.0-only
//! `_NET_WM_SYNC_REQUEST` (EWMH, application window sync).
//!
//! When the compositor resizes a managed X11 window, a client that lists
//! `_NET_WM_SYNC_REQUEST` in `WM_PROTOCOLS` is asked, by ClientMessage, to
//! update its `_NET_WM_SYNC_REQUEST_COUNTER` to a value once it has drawn the
//! new size. smithay 0.7 keeps its X connection private and drops every
//! `WM_PROTOCOLS` atom but `WM_TAKE_FOCUS` and `WM_DELETE_WINDOW`, so this module
//! owns a second x11rb connection to the same display (the one
//! `publish_resource_manager` already opens).
//!
//! Only the request is sent. Presentation is never held for the client, and
//! the counter is not read back, so completion is not observed: a client that
//! never answers cannot stall the compositor.
//!
//! Everything here runs on the loop thread. The round trips are the two
//! `GetProperty`s on the first resize of a window and the atom interning at
//! connect; neither waits on the client.

use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, Window},
    rust_connection::RustConnection,
};

/// Per-window answer, cached on first resize: whether the client lists the
/// protocol, its counter (0 if none), and the last value sent (0 if none).
#[derive(Debug, Clone, Copy)]
struct Tracked {
    window: Window,
    supports: bool,
    counter: u32,
    pending: u64,
}

/// The sync-request sender. Owned by `XWaylandState`, dropped with the XWM.
pub struct SyncRequests {
    conn: RustConnection,
    sync_request: u32,
    wm_protocols: u32,
    counter_atom: u32,
    /// The last value handed out; the next request is [`next_value`] of it.
    last: u64,
    windows: Vec<Tracked>,
}

/// The next request value: even, incrementing by two, never zero (zero is
/// the "no request" marker in [`Tracked`]). Wraps past `u64::MAX` to 2.
pub fn next_value(last: u64) -> u64 {
    match last.wrapping_add(2) {
        0 => 2,
        v => v,
    }
}

/// Split a 64-bit counter value into the `(low, high)` data words of the
/// ClientMessage.
pub fn split(value: u64) -> (u32, u32) {
    (value as u32, (value >> 32) as u32)
}

/// Is `atom` in a `WM_PROTOCOLS` list?
pub fn lists_protocol(list: &[u32], atom: u32) -> bool {
    list.contains(&atom)
}

/// ClientMessage data for a sync request: `[protocol, timestamp, lo, hi, 0]`.
/// `CurrentTime` (0) is what the EWMH examples send.
pub fn request_data(protocol: u32, value: u64) -> [u32; 5] {
    let (lo, hi) = split(value);
    [protocol, x11rb::CURRENT_TIME, lo, hi, 0]
}

/// Should a configure send a request? Only a size change is a resize; a
/// move alone never needs the client to redraw.
pub fn wants_request(size_changed: bool, supports: bool) -> bool {
    size_changed && supports
}

impl SyncRequests {
    /// Open the second connection to `:dpy` and intern the atoms. Blocking,
    /// once, at XWM start.
    pub fn connect(dpy: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let (conn, _screen) = x11rb::connect(Some(&format!(":{dpy}")))?;
        let sync_request = conn.intern_atom(false, b"_NET_WM_SYNC_REQUEST")?.reply()?.atom;
        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS")?.reply()?.atom;
        let counter_atom = conn
            .intern_atom(false, b"_NET_WM_SYNC_REQUEST_COUNTER")?
            .reply()?
            .atom;
        Ok(Self {
            conn,
            sync_request,
            wm_protocols,
            counter_atom,
            last: 0,
            windows: Vec::new(),
        })
    }

    /// The compositor is about to configure `window`. `size_changed` is
    /// whether the new size differs from the one the client last had.
    pub fn on_configure(&mut self, window: Window, size_changed: bool) {
        if !size_changed {
            return;
        }
        let idx = match self.windows.iter().position(|t| t.window == window) {
            Some(i) => i,
            None => {
                let t = self.discover(window);
                self.windows.push(t);
                self.windows.len() - 1
            }
        };
        let t = self.windows[idx];
        if !wants_request(size_changed, t.supports) {
            return;
        }
        let value = next_value(self.last);
        let data = request_data(self.sync_request, value);
        let event = ClientMessageEvent::new(32, window, self.wm_protocols, data);
        if let Err(e) = self
            .conn
            .send_event(false, window, EventMask::NO_EVENT, event)
            .and_then(|_| self.conn.flush())
        {
            tracing::debug!(error = %e, window, "could not send _NET_WM_SYNC_REQUEST");
            return;
        }
        if t.pending != 0 {
            // The previous request is not answered yet; we do not wait for it.
            tracing::debug!(window, previous = t.pending, "sync request superseded");
        }
        self.last = value;
        self.windows[idx].pending = value;
        tracing::debug!(window, value, counter = t.counter, "sent _NET_WM_SYNC_REQUEST");
    }

    /// Drop the cached answer when the window goes away.
    pub fn forget(&mut self, window: Window) {
        self.windows.retain(|t| t.window != window);
    }

    /// Read `WM_PROTOCOLS` and the counter. A failed read means the window
    /// does not take the protocol.
    fn discover(&self, window: Window) -> Tracked {
        let protocols = self
            .conn
            .get_property(false, window, self.wm_protocols, AtomEnum::ATOM, 0, 2048)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect::<Vec<u32>>()))
            .unwrap_or_default();
        let counter = self
            .conn
            .get_property(false, window, self.counter_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .unwrap_or(0);
        Tracked {
            window,
            supports: lists_protocol(&protocols, self.sync_request),
            counter,
            pending: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_even_increasing_and_never_zero() {
        let mut last = 0;
        let mut seen = Vec::new();
        for _ in 0..4 {
            last = next_value(last);
            seen.push(last);
        }
        assert_eq!(seen, vec![2, 4, 6, 8]);
        assert!(seen.iter().all(|v| v % 2 == 0 && *v != 0));
    }

    #[test]
    fn value_wraps_past_max_without_hitting_zero() {
        // u64::MAX - 1 is even; +2 wraps to 0, which is the "none" marker.
        assert_eq!(next_value(u64::MAX - 1), 2);
        assert_eq!(next_value(u64::MAX - 3), u64::MAX - 1);
    }

    #[test]
    fn value_is_split_low_then_high() {
        assert_eq!(split(0x0000_0001_0000_0002), (2, 1));
        assert_eq!(split(u64::MAX), (u32::MAX, u32::MAX));
        assert_eq!(split(0), (0, 0));
    }

    #[test]
    fn client_message_data_layout() {
        let d = request_data(77, 0x0000_0003_0000_0004);
        assert_eq!(d, [77, x11rb::CURRENT_TIME, 4, 3, 0]);
    }

    #[test]
    fn protocol_list_membership() {
        // WM_DELETE_WINDOW=1, _NET_WM_SYNC_REQUEST=9.
        assert!(lists_protocol(&[1, 9], 9));
        assert!(!lists_protocol(&[1], 9));
        assert!(!lists_protocol(&[], 9));
    }

    #[test]
    fn only_a_size_change_on_a_supporting_window_sends() {
        assert!(wants_request(true, true));
        assert!(!wants_request(false, true));
        assert!(!wants_request(true, false));
        assert!(!wants_request(false, false));
    }
}
