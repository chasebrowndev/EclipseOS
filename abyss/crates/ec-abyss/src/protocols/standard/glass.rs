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

use ec_abyss_render::glass::{
    parse_clip, parse_joins, parse_panes, GlassState, JoinError, PaneSet, ParseError,
};
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

/// Version 3 adds `set_joins` (ADR 0130).
const VERSION: u32 = 3;
/// The first version with `set_joins`.
const JOINS_SINCE: u32 = 3;

/// The shell binaries that may bind the global. The glass is drawn under the
/// client's surface, so `ec-secret-prompt` gains no access to what is typed.
/// `ec-greeter-ui` is the login screen (ADR 0126); the kiosk compositor offers
/// the global to it like any other. `ec-lock` is the screen locker: while the
/// session is locked its panes are drawn over the wallpaper alone (ADR 0129).
/// `ec-setup` is the graphical installer (D-03 §5): a sheet of glass over the
/// wallpaper on the live medium.
pub const DEFAULT_ALLOW: [&str; 10] = [
    "ec-hyperion-bar",
    "ec-launcher",
    "ec-toasts",
    "ec-settings",
    "ec-policy-viewer",
    "ec-secret-prompt",
    "apotheosis",
    "ec-greeter-ui",
    "ec-lock",
    "ec-setup",
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
        state: &mut Self,
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
                    Ok(mut set) => with_states(&surface, |states| {
                        let mut cached = states.cached_state.get::<GlassState>();
                        // The clip outlives the list (ADR 0127).
                        set.clip = cached.pending().panes.as_ref().and_then(|p| p.clip);
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
            SurfaceRequest::SetJoins { joins } => {
                if !data.owner {
                    return;
                }
                let Ok(surface) = data.surface.upgrade() else {
                    return;
                };
                // A v2 object has no `set_joins`: the request is not understood
                // and changes nothing (fail closed).
                if object.version() < JOINS_SINCE {
                    return;
                }
                // The joins index the pending pane list, which is the one they
                // latch with (`set_panes` clears them, so they follow it).
                let pending = with_states(&surface, |states| {
                    let mut cached = states.cached_state.get::<GlassState>();
                    cached.pending().panes.as_ref().map_or(0, |p| p.len())
                });
                match parse_joins(&joins, pending) {
                    Ok(set) => {
                        // Ignored while the compositor draws no panes: checked
                        // after validation, so a bad array still errors.
                        if !glass_capable(state) {
                            return;
                        }
                        with_states(&surface, |states| {
                            let mut cached = states.cached_state.get::<GlassState>();
                            cached
                                .pending()
                                .panes
                                .get_or_insert_with(PaneSet::default)
                                .set_joins(set);
                        });
                    }
                    Err(JoinError::BadJoin) => {
                        object.post_error(
                            ManagerError::BadJoin,
                            "a malformed joins array, a pane index out of range, or a pane joined to itself",
                        );
                    }
                    Err(JoinError::TooManyJoins) => {
                        object.post_error(ManagerError::TooManyJoins, "more than 64 joins");
                    }
                }
            }
            SurfaceRequest::SetClip { x, y, width, height } => {
                if !data.owner {
                    return;
                }
                let Ok(surface) = data.surface.upgrade() else {
                    return;
                };
                match parse_clip(x, y, width, height) {
                    Some(clip) => with_states(&surface, |states| {
                        let mut cached = states.cached_state.get::<GlassState>();
                        cached.pending().panes.get_or_insert_with(PaneSet::default).clip = clip;
                    }),
                    None => object.post_error(ManagerError::BadPane, "a negative clip size"),
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
    use ec_protocols::glass::{
        client::{
            eclipse_glass_manager_v1::{self, EclipseGlassManagerV1 as CManager},
            eclipse_glass_surface_v1::EclipseGlassSurfaceV1 as CSurface,
        },
        JoinRecord,
    };
    use std::os::unix::net::UnixStream;
    use wayland_client::{
        delegate_noop,
        protocol::{
            wl_compositor::WlCompositor, wl_registry, wl_subcompositor::WlSubcompositor,
            wl_subsurface::WlSubsurface, wl_surface::WlSurface as CWlSurface,
        },
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
    delegate_noop!(Seen: WlSubcompositor);
    delegate_noop!(Seen: ignore WlSubsurface);
    delegate_noop!(Seen: ignore CWlSurface);
    delegate_noop!(Seen: ignore CSurface);

    struct Client {
        server: smithay::reexports::wayland_server::Client,
        conn: Connection,
        queue: EventQueue<Seen>,
        seen: Seen,
        compositor: Option<WlCompositor>,
        subcompositor: Option<WlSubcompositor>,
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

        /// Connect at the current version (3).
        fn connect(h: &mut Harness, allowed: bool) -> Self {
            Self::connect_at(h, allowed, VERSION)
        }

        /// Connect, bind what the registry shows at `version`. The in-process
        /// client is this test binary, so `allowed` puts its name on (or off)
        /// the list.
        fn connect_at(h: &mut Harness, allowed: bool, version: u32) -> Self {
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
                subcompositor: None,
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
            let (n, v) = find(&c, "wl_subcompositor").unwrap();
            c.subcompositor = Some(registry.bind(n, v.min(1), &qh, ()));
            if let Some((n, _)) = find(&c, "eclipse_glass_manager_v1") {
                c.manager = Some(registry.bind(n, version, &qh, ()));
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

    /// ADR 0129: the screen locker may bind the global (its panes are drawn
    /// over the wallpaper while locked); a lookalike may not.
    #[test]
    fn ec_lock_is_allowlisted_and_lookalikes_are_not() {
        assert!(DEFAULT_ALLOW.contains(&"ec-lock"));
        let allow = Allowlist::new(DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect());
        assert!(allow.contains("ec-lock"));
        for name in ["ec-lock2", "ec-lockd", "lock", "swaylock", "ec-lock "] {
            assert!(!allow.contains(name), "{name}");
        }
        assert_eq!(DEFAULT_ALLOW.len(), 10);
    }

    /// The graphical installer (D-03 §5) binds the global; a lookalike may not.
    #[test]
    fn ec_setup_is_allowlisted_and_lookalikes_are_not() {
        let allow = Allowlist::new(DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect());
        assert!(allow.contains("ec-setup"));
        for name in ["ec-setup-helper", "ec-setup2", "setup"] {
            assert!(!allow.contains(name), "{name}");
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

    fn current_joins(surface: &WlSurface) -> Option<usize> {
        with_states(surface, |states| {
            let mut cached = states.cached_state.get::<GlassState>();
            cached.current().panes.as_ref().map(|p| p.joins().len())
        })
    }

    fn join_rec(a: u32, b: u32, radius: u32) -> Vec<u8> {
        JoinRecord { a, b, radius }.to_bytes().to_vec()
    }

    /// A glass-mode harness with a surface that has two panes latched, and
    /// the client's surface and object.
    fn glass_pair(h: &mut Harness, version: u32) -> (Client, CSurface, CWlSurface) {
        h.state.config.decoration.blur.mode = BlurMode::Glass;
        update_capabilities(&mut h.state);
        let mut c = Client::connect_at(h, true, version);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
        c.pump(h);
        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        surface.commit();
        c.pump(h);
        (c, glass, surface)
    }

    /// ADR 0130: joins are double-buffered with the panes and latch on commit.
    #[test]
    fn joins_latch_on_commit_and_set_panes_clears_them() {
        let mut h = harness();
        let (mut c, glass, surface) = glass_pair(&mut h, VERSION);
        let server = c.server_surface(&h, &surface);
        assert_eq!(current_joins(&server), Some(0));
        glass.set_joins(join_rec(0, 1, 16));
        c.pump(&mut h);
        assert_eq!(current_joins(&server), Some(0), "pending until the commit");
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current_joins(&server), Some(1));
        assert_eq!(c.error(), None);

        // The next pane list clears the joins, whatever they joined.
        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(2));
        assert_eq!(current_joins(&server), Some(0), "set_panes clears the joins");

        // An empty array clears them too.
        glass.set_joins(join_rec(0, 1, 8));
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current_joins(&server), Some(1));
        glass.set_joins(Vec::new());
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current_joins(&server), Some(0));
        assert_eq!(c.error(), None);
    }

    /// `bad_join` for a short record, an index past the pane list, and a pane
    /// joined to itself; `too_many_joins` past 64. Errors, not panics.
    #[test]
    fn bad_joins_are_protocol_errors() {
        let cases: [(Vec<u8>, ManagerError); 5] = [
            (vec![0u8; 11], ManagerError::BadJoin),
            (join_rec(0, 2, 8), ManagerError::BadJoin),
            (join_rec(1, 1, 8), ManagerError::BadJoin),
            (vec![0u8; 12 * 65], ManagerError::TooManyJoins),
            (join_rec(0, 0xffff_ffff, 8), ManagerError::BadJoin),
        ];
        for (bytes, code) in cases {
            let mut h = harness();
            let (mut c, glass, _) = glass_pair(&mut h, VERSION);
            glass.set_joins(bytes);
            c.pump(&mut h);
            assert_eq!(c.error(), Some(code as u32));
        }
    }

    /// A join that is valid against the pending list latches; one that names
    /// an index past the pending list is refused even if a longer list is
    /// current (the joins index what they are latched with).
    #[test]
    fn joins_are_checked_against_the_pending_list() {
        let mut h = harness();
        let (mut c, glass, _) = glass_pair(&mut h, VERSION);
        glass.set_panes(rec(0, 0));
        glass.set_joins(join_rec(0, 1, 8));
        c.pump(&mut h);
        assert_eq!(c.error(), Some(ManagerError::BadJoin as u32));
    }

    /// A v2 object has no `set_joins` (`since` 3): wayland-backend refuses the
    /// request at the wire with `wl_display.invalid_method` (code 1) before the
    /// handler runs, so a v2 client never gets joins latched. The handler's own
    /// version check is the second line of the same rule.
    #[test]
    fn a_v2_object_never_latches_joins() {
        let mut h = harness();
        h.state.config.decoration.blur.mode = BlurMode::Glass;
        update_capabilities(&mut h.state);
        let mut c = Client::connect_at(&mut h, true, 2);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
        c.pump(&mut h);
        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        surface.commit();
        c.pump(&mut h);
        assert_eq!(c.error(), None, "set_panes is version 1");
        glass.set_joins(join_rec(0, 1, 16));
        c.pump(&mut h);
        c.pump(&mut h);
        // The client is dropped for the request, so either its surface is
        // gone or it holds no joins: nothing is latched.
        let server = alive_surface(&c, &h, &surface);
        if let Some(server) = server {
            assert_eq!(current_joins(&server), Some(0));
        }
    }

    /// The server-side surface of `surface` if its client is still connected.
    fn alive_surface(c: &Client, h: &Harness, surface: &CWlSurface) -> Option<WlSurface> {
        c.server
            .object_from_protocol_id::<WlSurface>(&h.state.display_handle, surface.id().protocol_id())
            .ok()
    }

    /// The global advertises version 3 to a bind at 3.
    #[test]
    fn the_global_is_version_3() {
        let mut h = harness();
        let c = Client::connect(&mut h, true);
        let v = c
            .seen
            .globals
            .iter()
            .find(|(_, i, _)| i == "eclipse_glass_manager_v1")
            .map(|(_, _, v)| *v);
        assert_eq!(v, Some(3));
    }

    /// Joins are ignored while the compositor draws no panes: a valid array
    /// latches nothing, a bad one still errors.
    #[test]
    fn joins_are_ignored_while_capability_is_0() {
        let mut h = harness();
        h.state.config.decoration.blur.mode = BlurMode::Off;
        update_capabilities(&mut h.state);
        let mut c = Client::connect(&mut h, true);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
        c.pump(&mut h);
        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        glass.set_joins(join_rec(0, 1, 16));
        surface.commit();
        c.pump(&mut h);
        let server = c.server_surface(&h, &surface);
        assert_eq!(current(&server), Some(2));
        assert_eq!(current_joins(&server), Some(0));
        assert_eq!(c.error(), None);
        glass.set_joins(join_rec(0, 9, 16));
        c.pump(&mut h);
        assert_eq!(c.error(), Some(ManagerError::BadJoin as u32));
    }

    fn current_clip(surface: &WlSurface) -> Option<(i32, i32, i32, i32)> {
        with_states(surface, |states| {
            let mut cached = states.cached_state.get::<GlassState>();
            cached
                .current()
                .panes
                .as_ref()
                .and_then(|p| p.clip)
                .map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
        })
    }

    /// ADR 0127: `set_clip` is double-buffered with the panes, survives a new
    /// `set_panes`, and a zero size clears it.
    #[test]
    fn set_clip_latches_on_commit_survives_set_panes_and_clears_on_zero() {
        let mut h = harness();
        let mut c = Client::connect(&mut h, true);
        let qh = c.queue.handle();
        let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
        c.pump(&mut h);
        let server = c.server_surface(&h, &surface);
        glass.set_panes(rec(0, 0));
        glass.set_clip(5, 6, 70, 80);
        c.pump(&mut h);
        assert_eq!(current_clip(&server), None, "pending until the commit");
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current_clip(&server), Some((5, 6, 70, 80)));
        // New panes keep the clip.
        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(2));
        assert_eq!(current_clip(&server), Some((5, 6, 70, 80)));
        // Zero width or height clears it.
        glass.set_clip(0, 0, 0, 40);
        surface.commit();
        c.pump(&mut h);
        assert_eq!(current_clip(&server), None);
        assert_eq!(c.error(), None);
    }

    #[test]
    fn a_negative_clip_is_bad_pane() {
        for (w, h) in [(-1, 10), (10, -1)] {
            let mut hn = harness();
            let mut c = Client::connect(&mut hn, true);
            let qh = c.queue.handle();
            let surface = c.compositor.as_ref().unwrap().create_surface(&qh, ());
            let glass = c.manager.clone().unwrap().get_glass(&surface, &qh, ());
            c.pump(&mut hn);
            glass.set_clip(0, 0, w, h);
            c.pump(&mut hn);
            assert_eq!(c.error(), Some(ManagerError::BadPane as u32));
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

    /// COMP-02 §9: a subsurface may carry its own glass object. Its panes
    /// latch like any other surface state: a synchronized subsurface holds
    /// them until the parent commits, so a sheet and the frame under it land
    /// together; a desynchronized one latches on its own commit.
    #[test]
    fn a_subsurface_takes_glass_and_latches_with_its_mode() {
        let mut h = harness();
        let mut c = Client::connect(&mut h, true);
        let qh = c.queue.handle();
        let parent = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let child = c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let sub = c
            .subcompositor
            .clone()
            .unwrap()
            .get_subsurface(&child, &parent, &qh, ());
        let glass = c.manager.clone().unwrap().get_glass(&child, &qh, ());
        glass.set_panes(rec(0, 0));
        c.pump(&mut h);
        assert_eq!(c.error(), None);
        let server = c.server_surface(&h, &child);

        // Synchronized (the default): held until the parent commits.
        child.commit();
        c.pump(&mut h);
        assert_eq!(
            current(&server),
            None,
            "a synchronized child waits for its parent"
        );
        parent.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(1));

        // Desynchronized: its own commit latches.
        sub.set_desync();
        glass.set_panes([rec(0, 0), rec(30, 1)].concat());
        child.commit();
        c.pump(&mut h);
        assert_eq!(current(&server), Some(2));
        assert_eq!(c.error(), None);
    }
}
