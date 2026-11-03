// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_glass_v1` (COMP-02 §9): a DE client asks the compositor to draw
//! glass panes under parts of its own layer surface.
//!
//! Same shape as `background_effect.rs`. The pane list is double-buffered
//! surface state ([`ec_abyss_render::glass::GlassState`]): requests write the
//! pending side, `wl_surface.commit` latches it, and the renderer reads the
//! current side straight from the surface. `None` is "no object"; a live
//! object starts with an empty list; destroying the object clears the panes on
//! the next commit.
//!
//! Bind is for DE clients only: the global's `can_view` filter admits just the
//! shell binaries (by `exe` name, as screencopy does), so no other client even
//! sees it. The `capability` event is 1 exactly while `decoration.blur.mode`
//! is `glass`; [`update_capabilities`] re-announces it after a config change,
//! and clients fall back to their own ground when it is 0.

use std::sync::atomic::{AtomicBool, Ordering};

use ec_abyss_render::glass::{parse_panes, GlassState, PaneSet, ParseError};
use ec_protocols::glass::server::{
    eclipse_glass_manager_v1::{EclipseGlassManagerV1, Error as ManagerError, Request as ManagerRequest},
    eclipse_glass_surface_v1::{EclipseGlassSurfaceV1, Request as SurfaceRequest},
};
use smithay::{
    reexports::wayland_server::{
        backend::{ClientId, GlobalId},
        protocol::wl_surface::WlSurface,
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
    },
    wayland::compositor::with_states,
};

use crate::{
    config::{Allowlist, BlurMode},
    protocols::standard::data_control::WeakDh,
    state::AbyssState,
};

const VERSION: u32 = 1;

/// The shell binaries that may bind the global. `ec-secret-prompt` is left out
/// on purpose: it has no glass need.
pub const DEFAULT_ALLOW: [&str; 6] = [
    "ec-hyperion-bar",
    "ec-launcher",
    "ec-center",
    "ec-toasts",
    "ec-settings",
    "ec-policy-viewer",
];

/// On a surface's data map: whether it already has a glass object, which is
/// what `glass_exists` is raised from.
#[derive(Default)]
struct HasObject(AtomicBool);

/// Global data: enough to identify a client at bind time.
#[derive(Debug)]
pub struct ManagerData {
    dh: WeakDh,
    allow: Allowlist,
}

/// User data on an `eclipse_glass_surface_v1`.
pub struct GlassData {
    surface: Weak<WlSurface>,
    /// False for the duplicate that got `glass_exists`: it must not clear the
    /// flag the first object owns when it is destroyed.
    owner: bool,
}

pub struct GlassGlobals {
    #[allow(dead_code)] // holds the global alive
    global: GlobalId,
    instances: Vec<EclipseGlassManagerV1>,
    /// The capability last announced.
    available: bool,
    /// Who may bind. Shared with the global's filter.
    pub allow: Allowlist,
}

impl GlassGlobals {
    pub fn new(display: &DisplayHandle, dh: WeakDh, available: bool) -> Self {
        let allow = Allowlist::new(DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect());
        let global = display.create_global::<AbyssState, EclipseGlassManagerV1, _>(
            VERSION,
            ManagerData {
                dh,
                allow: allow.clone(),
            },
        );
        Self {
            global,
            instances: Vec::new(),
            available,
            allow,
        }
    }
}

/// Panes are drawn only in `glass` mode.
fn glass_capable(state: &AbyssState) -> bool {
    state.config.decoration.blur.mode == BlurMode::Glass
}

/// Re-announce the capability to every manager if the mode changed. Call
/// after a config change.
pub fn update_capabilities(state: &mut AbyssState) {
    let available = glass_capable(state);
    let globals = &mut state.glass;
    globals.instances.retain(|m| m.is_alive());
    if globals.available == available {
        return;
    }
    globals.available = available;
    for manager in &globals.instances {
        manager.capability(available as u32);
    }
}

impl GlobalDispatch<EclipseGlassManagerV1, ManagerData> for AbyssState {
    fn bind(
        state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<EclipseGlassManagerV1>,
        _data: &ManagerData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(manager, ());
        manager.capability(glass_capable(state) as u32);
        state.glass.instances.push(manager);
    }

    fn can_view(client: Client, data: &ManagerData) -> bool {
        // Fail-closed: an unknown client, or an empty list, sees nothing.
        let name = data.dh.client_name(&client);
        let allowed = name.as_deref().is_some_and(|n| data.allow.contains(n));
        if !allowed {
            tracing::debug!(
                client = name.as_deref().unwrap_or("<unknown>"),
                "eclipse_glass_v1 hidden"
            );
        }
        allowed
    }
}

impl Dispatch<EclipseGlassManagerV1, ()> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        manager: &EclipseGlassManagerV1,
        request: ManagerRequest,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ManagerRequest::GetGlass { id, surface } => {
                let owner = with_states(&surface, |states| {
                    let has = states.data_map.get_or_insert_threadsafe(HasObject::default);
                    !has.0.swap(true, Ordering::SeqCst)
                });
                if owner {
                    // A new object starts with no panes; that takes effect at
                    // the next commit like any other pending state.
                    with_states(&surface, |states| {
                        let mut cached = states.cached_state.get::<GlassState>();
                        cached.pending().panes = Some(PaneSet::default());
                    });
                } else {
                    manager.post_error(
                        ManagerError::GlassExists,
                        "the surface already has a glass object",
                    );
                }
                data_init.init(
                    id,
                    GlassData {
                        surface: surface.downgrade(),
                        owner,
                    },
                );
            }
            ManagerRequest::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &EclipseGlassManagerV1, _data: &()) {
        state.glass.instances.retain(|m| m != manager);
    }
}

impl Dispatch<EclipseGlassSurfaceV1, GlassData> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        object: &EclipseGlassSurfaceV1,
        request: SurfaceRequest,
        data: &GlassData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            SurfaceRequest::SetPanes { panes } => {
                if !data.owner {
                    return;
                }
                let Ok(surface) = data.surface.upgrade() else {
                    return;
                };
                match parse_panes(&panes) {
                    Ok(set) => with_states(&surface, |states| {
                        let mut cached = states.cached_state.get::<GlassState>();
                        cached.pending().panes = Some(set);
                    }),
                    Err(ParseError::BadPane) => {
                        object.post_error(
                            ManagerError::BadPane,
                            "a malformed panes array, unknown tone, or negative size",
                        );
                    }
                    Err(ParseError::TooManyPanes) => {
                        object.post_error(ManagerError::TooManyPanes, "more than 64 panes");
                    }
                }
            }
            SurfaceRequest::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(_state: &mut Self, _client: ClientId, _object: &EclipseGlassSurfaceV1, data: &GlassData) {
        // Explicit destroy or the client going away: the panes are removed on
        // the next commit, and the surface may get a new object.
        if !data.owner {
            return;
        }
        let Ok(surface) = data.surface.upgrade() else {
            return;
        };
        with_states(&surface, |states| {
            states.cached_state.get::<GlassState>().pending().panes = None;
            if let Some(has) = states.data_map.get::<HasObject>() {
                has.0.store(false, Ordering::SeqCst);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::focus::state_tests::{harness, Harness};
    use ec_protocols::glass::client::{
        eclipse_glass_manager_v1::{self, EclipseGlassManagerV1 as CManager},
        eclipse_glass_surface_v1::EclipseGlassSurfaceV1 as CSurface,
    };
    use std::os::unix::net::UnixStream;
    use wayland_client::{
        delegate_noop,
        protocol::{wl_compositor::WlCompositor, wl_registry, wl_surface::WlSurface as CWlSurface},
        Connection, Dispatch as CDispatch, EventQueue, Proxy, QueueHandle,
    };

    #[derive(Default)]
    struct Seen {
        globals: Vec<(u32, String, u32)>,
        caps: Vec<u32>,
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

    impl CDispatch<CManager, ()> for Seen {
        fn event(
            s: &mut Self,
            _: &CManager,
            e: eclipse_glass_manager_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let eclipse_glass_manager_v1::Event::Capability { available } = e {
                s.caps.push(available);
            }
        }
    }

    delegate_noop!(Seen: WlCompositor);
    delegate_noop!(Seen: ignore CWlSurface);
    delegate_noop!(Seen: ignore CSurface);

    struct Client {
        server: smithay::reexports::wayland_server::Client,
        conn: Connection,
        queue: EventQueue<Seen>,
        seen: Seen,
        compositor: Option<WlCompositor>,
        manager: Option<CManager>,
    }

    impl Client {
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

        /// Connect, bind what the registry shows. The in-process client is
        /// this test binary, so `allowed` puts its name on (or off) the list.
        fn connect(h: &mut Harness, allowed: bool) -> Self {
            let me = std::env::current_exe().unwrap();
            let me = me.file_name().unwrap().to_string_lossy().into_owned();
            h.state
                .glass
                .allow
                .set(if allowed { vec![me] } else { Vec::new() });
            let (server, client) = UnixStream::pair().unwrap();
            let server = h
                .state
                .display_handle
                .insert_client(server, crate::state::client_state())
                .unwrap();
            let conn = Connection::from_socket(client).unwrap();
            let queue = conn.new_event_queue();
            let qh = queue.handle();
            let registry = conn.display().get_registry(&qh, ());
            let mut c = Self {
                server,
                conn,
                queue,
                seen: Seen::default(),
                compositor: None,
                manager: None,
            };
            c.pump(h);
            let find = |c: &Client, name: &str| {
                c.seen
                    .globals
                    .iter()
                    .find(|(_, i, _)| i == name)
                    .map(|(n, _, v)| (*n, *v))
            };
            let (n, v) = find(&c, "wl_compositor").unwrap();
            c.compositor = Some(registry.bind(n, v.min(5), &qh, ()));
            if let Some((n, _)) = find(&c, "eclipse_glass_manager_v1") {
                c.manager = Some(registry.bind(n, 1, &qh, ()));
            }
            c.pump(h);
            c
        }

        fn error(&self) -> Option<u32> {
            self.conn.protocol_error().map(|e| e.code)
        }
    }

    fn rec(x: i32, tone: u32) -> Vec<u8> {
        [x as u32, 0, 20, 10, 4, 0, tone, 0, 255]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect()
    }

    fn current(surface: &WlSurface) -> Option<usize> {
        with_states(surface, |states| {
            let mut cached = states.cached_state.get::<GlassState>();
            cached.current().panes.as_ref().map(|p| p.len())
        })
    }

    impl Client {
        /// The server-side `wl_surface` the client's `surface` is.
        fn server_surface(&self, h: &Harness, surface: &CWlSurface) -> WlSurface {
            self.server
                .object_from_protocol_id::<WlSurface>(&h.state.display_handle, surface.id().protocol_id())
                .expect("server surface")
        }
    }

    #[test]
    fn only_allowlisted_clients_see_the_global() {
        let mut h = harness();
        assert!(Client::connect(&mut h, false).manager.is_none());
        assert!(Client::connect(&mut h, true).manager.is_some());
    }

    #[test]
    fn capability_follows_the_blur_mode() {
        let mut h = harness();
        h.state.config.decoration.blur.mode = BlurMode::Glass;
        update_capabilities(&mut h.state);
        let mut c = Client::connect(&mut h, true);
        assert_eq!(c.seen.caps, vec![1]);
        h.state.config.decoration.blur.mode = BlurMode::Frost;
        update_capabilities(&mut h.state);
        c.pump(&mut h);
        assert_eq!(c.seen.caps, vec![1, 0]);
        // No change, no event.
        update_capabilities(&mut h.state);
        c.pump(&mut h);
        assert_eq!(c.seen.caps, vec![1, 0]);
        h.state.config.decoration.blur.mode = BlurMode::Glass;
        update_capabilities(&mut h.state);
        c.pump(&mut h);
        assert_eq!(c.seen.caps, vec![1, 0, 1]);
        // A bind while it is off hears 0.
        h.state.config.decoration.blur.mode = BlurMode::Off;
        update_capabilities(&mut h.state);
        let late = Client::connect(&mut h, true);
        assert_eq!(late.seen.caps, vec![0]);
    }

    #[test]
    fn a_second_object_is_glass_exists() {
        let mut h = harness();
        let mut c = Client::connect(&mut h, true);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let m = c.manager.clone().unwrap();
        let _a = m.get_glass(&surface, &qh, ());
        c.pump(&mut h);
        assert_eq!(c.error(), None);
        let _b = m.get_glass(&surface, &qh, ());
        c.pump(&mut h);
        assert_eq!(c.error(), Some(ManagerError::GlassExists as u32));
    }

    #[test]
    fn panes_latch_on_commit_and_clear_with_the_object() {
        let mut h = harness();
        let mut c = Client::connect(&mut h, true);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
        c.pump(&mut h);
        let server = c.server_surface(&h, &surface);
        // A fresh surface has no glass state until it commits.
        assert_eq!(current(&server), None);
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(0), "a new object starts with no panes");

        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        c.pump(&mut h);
        assert_eq!(current(&server), Some(0), "pending until the commit");
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(2));
        assert_eq!(c.error(), None);

        glass.destroy();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(2), "removal is also latched on commit");
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), None);
    }

    #[test]
    fn bad_arrays_are_protocol_errors() {
        for (bytes, code) in [
            (vec![0u8; 35], ManagerError::BadPane),
            (rec(0, 9), ManagerError::BadPane),
            (
                [0u32, 0, 0xffff_ffff, 1, 0, 0, 0, 0, 0]
                    .iter()
                    .flat_map(|v| v.to_ne_bytes())
                    .collect(),
                ManagerError::BadPane,
            ),
            (vec![0u8; 36 * 65], ManagerError::TooManyPanes),
        ] {
            let mut h = harness();
            let mut c = Client::connect(&mut h, true);
            let qh = c.queue.handle();
            let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
            let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
            c.pump(&mut h);
            glass.set_panes(bytes);
            c.pump(&mut h);
            assert_eq!(c.error(), Some(code as u32));
        }
    }

    #[test]
    fn a_surface_without_glass_has_no_state() {
        let mut h = harness();
        let mut c = Client::connect(&mut h, true);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current(&c.server_surface(&h, &surface)), None);
    }
}
