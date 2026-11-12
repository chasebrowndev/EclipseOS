// SPDX-License-Identifier: AGPL-3.0-only
//! Executable-path admission to the human-path security globals (ADR 0094,
//! COMP-06 §2, §5): a client not on the list does not see the global and
//! cannot bind it, and the same basename at another path is not on the list.
//!
//! The in-process client is this test binary, so "allowlisted" means its own
//! resolved path is the entry.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use wayland_client::{
    delegate_noop, protocol::wl_registry, Connection, Dispatch as CDispatch, EventQueue, QueueHandle,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;

use crate::shell::focus::state_tests::{harness, Harness};

const VIRTUAL_POINTER: &str = "zwlr_virtual_pointer_manager_v1";
const INPUT_METHOD: &str = "zwp_input_method_manager_v2";
const DATA_CONTROL: &str = "zwlr_data_control_manager_v1";
const SCREENCOPY: &str = "zwlr_screencopy_manager_v1";
const IMAGE_COPY: &str = "ext_image_copy_capture_manager_v1";

#[derive(Default)]
struct Seen {
    globals: Vec<(u32, String, u32)>,
}

impl CDispatch<wl_registry::WlRegistry, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            s.globals.push((name, interface, version));
        }
    }
}

delegate_noop!(Seen: ZwlrVirtualPointerManagerV1);

struct Client {
    conn: Connection,
    queue: EventQueue<Seen>,
    registry: wl_registry::WlRegistry,
    seen: Seen,
}

impl Client {
    fn connect(h: &mut Harness) -> Self {
        let (server, client) = UnixStream::pair().unwrap();
        h.state
            .display_handle
            .insert_client(server, crate::state::client_state())
            .unwrap();
        let conn = Connection::from_socket(client).unwrap();
        let queue = conn.new_event_queue();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut c = Self {
            conn,
            queue,
            registry,
            seen: Seen::default(),
        };
        c.pump(h);
        c
    }

    fn pump(&mut self, h: &mut Harness) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            h.dispatch();
            if let Some(g) = self.conn.prepare_read() {
                let _ = g.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.seen);
        }
    }

    fn global(&self, interface: &str) -> Option<u32> {
        self.seen
            .globals
            .iter()
            .find(|(_, i, _)| i == interface)
            .map(|(n, _, _)| *n)
    }
}

/// This test binary, resolved the way `/proc/<pid>/exe` reports it.
fn me() -> PathBuf {
    std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
}

/// Put `entries` on every list this module covers.
fn admit(h: &mut Harness, entries: Vec<String>) {
    h.state.capture_allow.set(entries.clone());
    h.state.clipboard_allow.set(entries.clone());
    h.state.input_method_allow.set(entries.clone());
    h.state.virtual_pointer.allow.set(entries);
}

const GATED: [&str; 5] = [
    VIRTUAL_POINTER,
    INPUT_METHOD,
    DATA_CONTROL,
    SCREENCOPY,
    IMAGE_COPY,
];

#[test]
fn by_default_no_gated_global_is_visible() {
    let mut h = harness();
    let c = Client::connect(&mut h);
    // The display is live: ordinary globals are there.
    assert!(c.global("wl_compositor").is_some());
    for g in GATED {
        assert!(c.global(g).is_none(), "{g} visible with an empty allowlist");
    }
}

#[test]
fn the_allowlisted_executable_sees_them() {
    let mut h = harness();
    admit(&mut h, vec![me().to_string_lossy().into_owned()]);
    let c = Client::connect(&mut h);
    for g in GATED {
        assert!(c.global(g).is_some(), "{g} hidden from an allowlisted client");
    }
}

/// SEC-04: the entry is the executable, not its basename. A bare name means
/// `/usr/bin/<name>`, which this test binary is not, and a path to a copy of
/// it under another directory names a different file.
#[test]
fn the_same_basename_elsewhere_is_not_admitted() {
    let mut h = harness();
    let name = me().file_name().unwrap().to_string_lossy().into_owned();
    let copy = std::env::temp_dir().join(&name);
    for entry in [name.clone(), copy.to_string_lossy().into_owned()] {
        admit(&mut h, vec![entry.clone()]);
        let c = Client::connect(&mut h);
        for g in GATED {
            assert!(c.global(g).is_none(), "{g} visible for entry {entry}");
        }
    }
}

/// A hidden global cannot be bound by guessing its name: wayland-server
/// re-runs the filter at bind and kills the client.
#[test]
fn binding_a_hidden_global_is_a_protocol_error() {
    let mut h = harness();
    admit(&mut h, vec![me().to_string_lossy().into_owned()]);
    let name = Client::connect(&mut h)
        .global(VIRTUAL_POINTER)
        .expect("visible when admitted");

    admit(&mut h, Vec::new());
    let mut c = Client::connect(&mut h);
    assert!(c.global(VIRTUAL_POINTER).is_none());
    let qh = c.queue.handle();
    let _m: ZwlrVirtualPointerManagerV1 = c.registry.bind(name, 1, &qh, ());
    c.pump(&mut h);
    assert!(
        c.conn.protocol_error().is_some(),
        "bind to a hidden global was accepted"
    );
}

/// A reload that empties the list hides the global from the next client.
#[test]
fn a_reload_revokes_for_new_clients() {
    let mut h = harness();
    admit(&mut h, vec![me().to_string_lossy().into_owned()]);
    assert!(Client::connect(&mut h).global(INPUT_METHOD).is_some());
    admit(&mut h, Vec::new());
    assert!(Client::connect(&mut h).global(INPUT_METHOD).is_none());
}
