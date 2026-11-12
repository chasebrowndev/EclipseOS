// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_foreign_toplevel_list_v1` (COMP-06 §1) — the read-only window list a
//! taskbar or a switcher needs, and the one `registryd` reads.
//!
//! Read-only is the whole point: this protocol lists titles, app ids and stable
//! identifiers, and offers no way to focus, close or move anything. The
//! management protocols that do offer that (`wlr_foreign_toplevel_management`)
//! are deliberately absent (COMP-06 §2) — window control goes through the
//! audited agent protocol, not an ambient global.
//!
//! The global itself is open to every client: third-party bars need it. What
//! it never carries is a `secret` window (S-05 §2: `secret` windows never
//! reach registryd, which reads this list). A window that is `secret` when it
//! maps is never published; one raised to `secret` later is withdrawn (a
//! `closed` event) *before* the title that raised it would have been sent;
//! one that drops below `secret` again is published afresh, under a new
//! identifier (ADR 0094).
//!
//! Entries are owned by the window (`Window::user_data`), so a window's entry
//! dies exactly when the window does.

use std::cell::RefCell;

use ec_policy_eval::Class;
use smithay::{
    desktop::Window,
    wayland::foreign_toplevel_list::{
        ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
    },
};

use crate::state::AbyssState;

impl ForeignToplevelListHandler for AbyssState {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list
    }
}

/// On a window's user data from map to close. `handle` is `None` while the
/// window is withheld; `title`/`app_id` are the last values seen either way,
/// so a title change is still reported to the caller (the IPC `window` event)
/// for a withheld window.
#[derive(Default)]
struct Entry(RefCell<Slot>);

#[derive(Default)]
struct Slot {
    handle: Option<ForeignToplevelHandle>,
    title: String,
    app_id: String,
}

/// Identity as the rest of the compositor sees it (see `ipc::methods`).
fn identity(window: &Window) -> (String, String) {
    let (app_id, title) = crate::ipc::methods::identity_of(window);
    (title.unwrap_or_default(), app_id.unwrap_or_default())
}

/// Whether `window` is `secret` for this human-path global.
///
/// The same rule capture redaction applies (`render::capture::classify`),
/// built from the same inputs, so the two cannot disagree about a window:
/// the compositor's sensitive set (which `sensitivity` window rules and the
/// capture-policy classifier write), `capture.redact-app-id`, and — once
/// `policyd` has pushed a table — the policy class from
/// [`crate::policy::classes::class_of`]. With no table `class_of` reports
/// every window `secret` (S-05 §8, the agent-facing answer); applied here
/// that would empty every taskbar, so, exactly as capture does, only the
/// compositor's own set counts then.
fn secret(state: &AbyssState, window: &Window) -> bool {
    let surface = crate::shell::window_surface(window);
    if surface.as_ref().is_some_and(|s| state.sensitive.contains(s)) {
        return true;
    }
    let redacted = surface
        .as_ref()
        .and_then(crate::protocols::standard::data_device::app_id_of)
        .is_some_and(|id| state.config.capture.redact_app_id.contains(&id));
    redacted
        || (state.policy_table.is_some() && crate::policy::classes::class_of(state, window) == Class::Secret)
}

/// Announce a newly mapped window, unless it is `secret`. Called from the
/// map funnel after the window rules have run, so the XWayland path is
/// covered by the same code and a `sensitivity` rule is already applied.
pub fn window_mapped(state: &mut AbyssState, window: &Window) {
    let (title, app_id) = identity(window);
    let hide = secret(state, window);
    window.user_data().insert_if_missing(Entry::default);
    let Some(entry) = window.user_data().get::<Entry>() else {
        return;
    };
    let mut slot = entry.0.borrow_mut();
    if slot.handle.is_none() && !hide {
        slot.handle = Some(
            state
                .foreign_toplevel_list
                .new_toplevel::<AbyssState>(title.clone(), app_id.clone()),
        );
    }
    slot.title = title;
    slot.app_id = app_id;
}

/// Re-check `window` after a commit or a property change: withdraw it if it
/// is now `secret`, publish it if it no longer is, otherwise push a
/// title/app_id change. Returns whether the title or app_id changed, whether
/// or not the window is published. Call after the rules have re-run.
pub fn window_updated(state: &mut AbyssState, window: &Window) -> bool {
    let Some(entry) = window.user_data().get::<Entry>() else {
        return false;
    };
    let hide = secret(state, window);
    let (title, app_id) = identity(window);
    let mut slot = entry.0.borrow_mut();
    let changed = slot.title != title || slot.app_id != app_id;
    match (slot.handle.take(), hide) {
        // Withdrawn before anything new is sent: the title that raised the
        // class never reaches a client.
        (Some(handle), true) => state.foreign_toplevel_list.remove_toplevel(&handle),
        (Some(handle), false) => {
            if changed {
                handle.send_title(&title);
                handle.send_app_id(&app_id);
                handle.send_done();
            }
            slot.handle = Some(handle);
        }
        (None, false) => {
            slot.handle = Some(
                state
                    .foreign_toplevel_list
                    .new_toplevel::<AbyssState>(title.clone(), app_id.clone()),
            );
        }
        (None, true) => {}
    }
    if changed {
        slot.title = title;
        slot.app_id = app_id;
    }
    changed
}

/// Re-check every mapped window. For class inputs that change without a
/// commit: a config reload (`capture.redact-app-id`, window rules) or a new
/// policy table.
pub fn refresh_all(state: &mut AbyssState) {
    for window in crate::shell::owned_windows(state) {
        let _ = window_updated(state, &window);
    }
}

/// The window is gone for good — not merely off the visible workspace.
pub fn window_closed(window: &Window) {
    if let Some(entry) = window.user_data().get::<Entry>() {
        if let Some(handle) = entry.0.borrow_mut().handle.take() {
            handle.send_closed();
        }
    }
}

/// Whether `window` is on the list right now. Tests only.
#[cfg(test)]
pub(crate) fn published(window: &Window) -> bool {
    window
        .user_data()
        .get::<Entry>()
        .is_some_and(|e| e.0.borrow().handle.is_some())
}

smithay::delegate_foreign_toplevel_list!(AbyssState);

#[cfg(test)]
mod tests {
    //! SEC-03: a `secret` window is never on the list, and one raised to
    //! `secret` is withdrawn before the raising title or app_id is sent.

    use super::published;
    use crate::shell::focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    };
    use smithay::desktop::Window;
    use std::os::unix::net::UnixStream;
    use wayland_client::{
        event_created_child, protocol::wl_registry, Connection, Dispatch as CDispatch, EventQueue, Proxy,
        QueueHandle,
    };
    use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    };

    const VAULT: &str = "ec.test.vault";

    /// What a third-party bar would see.
    #[derive(Default)]
    struct Seen {
        globals: Vec<(u32, String)>,
        toplevels: usize,
        closed: usize,
        titles: Vec<String>,
        app_ids: Vec<String>,
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
            if let wl_registry::Event::Global { name, interface, .. } = e {
                s.globals.push((name, interface));
            }
        }
    }

    impl CDispatch<ExtForeignToplevelListV1, ()> for Seen {
        fn event(
            s: &mut Self,
            _: &ExtForeignToplevelListV1,
            e: ext_foreign_toplevel_list_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let ext_foreign_toplevel_list_v1::Event::Toplevel { .. } = e {
                s.toplevels += 1;
            }
        }

        event_created_child!(Seen, ExtForeignToplevelListV1, [
            ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
        ]);
    }

    impl CDispatch<ExtForeignToplevelHandleV1, ()> for Seen {
        fn event(
            s: &mut Self,
            _: &ExtForeignToplevelHandleV1,
            e: ext_foreign_toplevel_handle_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match e {
                ext_foreign_toplevel_handle_v1::Event::Title { title } => s.titles.push(title),
                ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => s.app_ids.push(app_id),
                ext_foreign_toplevel_handle_v1::Event::Closed => s.closed += 1,
                _ => {}
            }
        }
    }

    struct Bar {
        conn: Connection,
        queue: EventQueue<Seen>,
        seen: Seen,
        _list: ExtForeignToplevelListV1,
    }

    impl Bar {
        fn connect(h: &mut Harness) -> Self {
            let (server, client) = UnixStream::pair().unwrap();
            h.state
                .display_handle
                .insert_client(server, crate::state::client_state())
                .unwrap();
            let conn = Connection::from_socket(client).unwrap();
            let mut queue = conn.new_event_queue();
            let qh = queue.handle();
            let registry = conn.display().get_registry(&qh, ());
            let mut seen = Seen::default();
            for _ in 0..8 {
                let _ = conn.flush();
                h.dispatch();
                if let Some(g) = conn.prepare_read() {
                    let _ = g.read();
                }
                let _ = queue.dispatch_pending(&mut seen);
            }
            let name = seen
                .globals
                .iter()
                .find(|(_, i)| i == "ext_foreign_toplevel_list_v1")
                .map(|(n, _)| *n)
                .expect("the list is open to every client");
            let list = registry.bind(name, 1, &qh, ());
            let mut bar = Self {
                conn,
                queue,
                seen,
                _list: list,
            };
            bar.pump(h);
            bar
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
    }

    fn window_of(h: &Harness, t: &Toplevel) -> Window {
        let id = t.surface.id().protocol_id();
        crate::shell::owned_windows(&h.state)
            .into_iter()
            .find(|w| {
                crate::shell::window_surface(w)
                    .is_some_and(|s| smithay::reexports::wayland_server::Resource::id(&s).protocol_id() == id)
            })
            .expect("mapped")
    }

    /// Map a toplevel with `app_id` and `title` set before the initial commit.
    fn map(h: &mut Harness, c: &mut Client, app_id: &str, title: &str) -> (Toplevel, Window) {
        let t = c.create_toplevel(h);
        t.toplevel.set_app_id(app_id.into());
        t.toplevel.set_title(title.into());
        c.commit(h, &t.surface);
        c.attach(h, &t.surface);
        let w = window_of(h, &t);
        (t, w)
    }

    #[test]
    fn a_secret_window_is_never_published() {
        let mut h = harness();
        h.state.config.capture.redact_app_id = vec![VAULT.into()];
        let mut bar = Bar::connect(&mut h);
        let mut c = Client::connect(&mut h);

        let (_v, vault) = map(&mut h, &mut c, VAULT, "vault: bank login");
        let (_p, plain) = map(&mut h, &mut c, "ec.test.plain", "notes");
        bar.pump(&mut h);

        assert!(!published(&vault));
        assert!(published(&plain));
        assert_eq!(bar.seen.toplevels, 1, "only the ordinary window is listed");
        assert_eq!(bar.seen.titles, ["notes"]);
        assert!(!bar.seen.app_ids.iter().any(|a| a == VAULT));
    }

    #[test]
    fn a_window_raised_to_secret_is_withdrawn_before_the_new_title_is_sent() {
        let mut h = harness();
        h.state.config.capture.redact_app_id = vec![VAULT.into()];
        let mut bar = Bar::connect(&mut h);
        let mut c = Client::connect(&mut h);

        let (t, window) = map(&mut h, &mut c, "ec.test.plain", "notes");
        bar.pump(&mut h);
        assert_eq!((bar.seen.toplevels, bar.seen.closed), (1, 0));

        // One commit carries the raising app_id and a title that must not leak.
        t.toplevel.set_app_id(VAULT.into());
        t.toplevel.set_title("vault: bank login".into());
        c.commit(&mut h, &t.surface);
        bar.pump(&mut h);

        assert!(!published(&window));
        assert_eq!(bar.seen.closed, 1, "withdrawn with a closed event");
        assert!(
            !bar.seen.titles.iter().any(|t| t.contains("bank")),
            "{:?}",
            bar.seen.titles
        );
        assert!(!bar.seen.app_ids.iter().any(|a| a == VAULT));
    }

    /// A raise that arrives without a commit — the sensitive set or a config
    /// reload — is applied by `refresh_all`.
    #[test]
    fn a_raise_without_a_commit_is_withdrawn_on_refresh() {
        let mut h = harness();
        let mut bar = Bar::connect(&mut h);
        let mut c = Client::connect(&mut h);

        let (_t, window) = map(&mut h, &mut c, "ec.test.plain", "notes");
        bar.pump(&mut h);
        assert!(published(&window));

        let surface = crate::shell::window_surface(&window).expect("surface");
        h.state.sensitive.insert(surface);
        super::refresh_all(&mut h.state);
        bar.pump(&mut h);

        assert!(!published(&window));
        assert_eq!(bar.seen.closed, 1);
    }
}
