// SPDX-License-Identifier: AGPL-3.0-only
//! The privileged agent socket and `eclipse_agent_v1` (COMP-08), M11 subset:
//! admission and the read-only scene queries.
//!
//! Not TCB, and decides nothing. Grants are admitted by
//! [`crate::policy::Agent`], and every window an agent learns about comes
//! out of [`crate::policy::scene`]. This module owns the socket, the Wayland
//! objects and the order the checks run in.
//!
//! - The socket is `$XDG_RUNTIME_DIR/eclipse/ec-agent.sock`, mode 0600
//!   (COMP-01 §5 step 10, F-06), or `ECLIPSE_AGENT_SOCKET` for a second
//!   compositor. Every peer is `SO_PEERCRED`-checked against our uid before
//!   it becomes a client (COMP-13 §3).
//! - It exists only while the `agents` hook is on (COMP-01 §6, ADR 0069).
//!   Turning the hook off disconnects every agent client, as "`agentd`
//!   dies" does, and then removes the socket.
//! - The socket is served by its own `Display`, created here, which holds
//!   the manager global and nothing else (C-00 §8.1, COMP-08 §3). An agent
//!   client therefore cannot see or bind any human global (toplevel lists,
//!   virtual input, data control, screencopy), and a `wayland-N` client
//!   neither sees nor can bind the manager. `can_view` on the manager stays
//!   as defence in depth.
//!
//! Agent objects live in [`Agents`], keyed by a `u64` the Wayland resource
//! carries as its user data. Nothing here is shared across threads.

use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;

use ec_protocols::agent::server::{
    eclipse_agent_manager_v1::{self, EclipseAgentManagerV1},
    eclipse_agent_v1::{self, EclipseAgentV1, Status},
    eclipse_scene_v1::{self, EclipseSceneV1},
};
use policy_eval::scope::SCENE_READ;
use policy_eval::Class;
use smithay::desktop::Window;
use smithay::reexports::calloop::{generic::Generic, Interest, Mode, PostAction, RegistrationToken};
use smithay::reexports::wayland_server::{
    backend::{ClientId, DisconnectReason, GlobalId},
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::addons::Hook;
use crate::audit;
use crate::policy::{scene, AdmitError, Agent, Views};
use crate::state::{AbyssState, ClientState};

#[cfg(test)]
mod leakage;
#[cfg(test)]
mod tests;

const MANAGER_VERSION: u32 = 1;

/// How long `(agent, req_id)` results are kept (COMP-08 §2.2 default).
/// Announced on bind; the dedupe table itself is not built yet.
const DEDUPE_WINDOW_S: u32 = 60;

/// The socket, the global and the live agent objects.
#[derive(Debug, Default)]
pub struct Agents {
    /// The bound socket file, to unlink on hook-off and on shutdown.
    path: Option<PathBuf>,
    listener: Option<RegistrationToken>,
    global: Option<GlobalId>,
    /// The agent socket's own display. Created on first start and kept, idle
    /// and unreachable, while the hook is off; its `Display` is owned by its
    /// dispatch source in the calloop loop.
    display: Option<DisplayHandle>,
    /// `(id, agent)`. The slot is `None` only while a request has the agent
    /// taken out, so the scene filter can borrow the state alongside it.
    slots: Vec<(u64, Option<Agent>)>,
    next_id: u64,
    /// Clients accepted on the socket, so hook-off can disconnect them.
    /// (wayland-backend's `with_all_clients` spins on the system backend.)
    clients: Vec<ClientId>,
    /// Where tests bind instead of the session's runtime directory.
    #[cfg(test)]
    pub(crate) test_path: Option<PathBuf>,
}

impl Agents {
    /// Whether the privileged socket is up.
    pub fn listening(&self) -> bool {
        self.path.is_some()
    }

    fn take(&mut self, id: u64) -> Option<Agent> {
        self.slots.iter_mut().find(|(i, _)| *i == id)?.1.take()
    }

    fn put(&mut self, id: u64, agent: Agent) {
        if let Some(slot) = self.slots.iter_mut().find(|(i, _)| *i == id) {
            slot.1 = Some(agent);
        }
    }
}

/// The agent object a resource speaks for. 0 is never minted: it marks the
/// object a refused `create_agent` had to initialise before its error.
#[derive(Debug, Clone, Copy)]
pub struct AgentId(u64);

fn socket_path(_agents: &Agents) -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(p) = &_agents.test_path {
        return Some(p.clone());
    }
    if let Some(p) = std::env::var_os("ECLIPSE_AGENT_SOCKET") {
        return Some(PathBuf::from(p));
    }
    Some(crate::ipc::socket_dir()?.join("ec-agent.sock"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Bring the socket and global in line with the `agents` hook. Called at
/// startup and on every add-on change.
pub fn sync(state: &mut AbyssState) {
    let on = state.addons.hooks.is_on(Hook::Agents);
    if on && !state.agents.listening() {
        start(state);
    } else if !on && state.agents.listening() {
        stop(state);
    }
}

/// The agent display, created and put on the loop the first time it is
/// needed. Nothing human is ever registered on it.
fn agent_display(state: &mut AbyssState) -> Option<DisplayHandle> {
    if let Some(dh) = &state.agents.display {
        return Some(dh.clone());
    }
    let display = match Display::<AbyssState>::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(?e, "creating the agent display");
            return None;
        }
    };
    let dh = display.handle();
    let inserted = state.loop_handle.insert_source(
        Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            // SAFETY: the display is only touched from this callback on the loop thread.
            unsafe { display.get_mut().dispatch_clients(state) }?;
            Ok(PostAction::Continue)
        },
    );
    if let Err(e) = inserted {
        tracing::warn!(%e, "inserting the agent display source");
        return None;
    }
    state.agents.display = Some(dh.clone());
    Some(dh)
}

/// Flush the agent display. Called only from each backend's event-loop
/// hook, beside the main display's flush: `wl_display_flush_clients` may
/// destroy a client, which is only safe outside request dispatch.
pub fn flush(state: &mut AbyssState) {
    if let Some(dh) = state.agents.display.as_mut() {
        let _ = dh.flush_clients();
    }
}

fn start(state: &mut AbyssState) {
    let Some(path) = socket_path(&state.agents) else {
        tracing::warn!("XDG_RUNTIME_DIR unset; no agent socket");
        return;
    };
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_socket() => {
            // A socket that answers belongs to another live compositor.
            if UnixStream::connect(&path).is_ok() {
                tracing::error!(path = %path.display(), "agent socket already in use; not binding");
                return;
            }
            let _ = std::fs::remove_file(&path);
        }
        // Never unlink anything that is not a socket, symlinks included.
        Ok(_) => {
            tracing::warn!(path = %path.display(), "agent socket path is not a socket; not binding");
            return;
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), %e, "inspecting the agent socket path");
            return;
        }
    }
    let Some(dh) = agent_display(state) else {
        return;
    };
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(path = %path.display(), %e, "binding the agent socket");
            return;
        }
    };
    // Mode 0600, owned by the user (COMP-01 §5 step 10). Anyone who connects
    // in the instant before this still meets the uid check in `accept`.
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::error!(path = %path.display(), %e, "tightening the agent socket; removing it");
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Err(e) = listener.set_nonblocking(true) {
        tracing::warn!(%e, "agent socket non-blocking");
        let _ = std::fs::remove_file(&path);
        return;
    }
    let inserted = state.loop_handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => accept(state, stream),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::warn!(%e, "agent socket accept");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    );
    let token = match inserted {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(%e, "inserting the agent socket source");
            let _ = std::fs::remove_file(&path);
            return;
        }
    };
    let global = dh.create_global::<AbyssState, EclipseAgentManagerV1, _>(MANAGER_VERSION, ());
    tracing::info!(path = %path.display(), "agent socket listening");
    state.agents.path = Some(path);
    state.agents.listener = Some(token);
    state.agents.global = Some(global);
}

/// Hook off: as "`agentd` dies" (COMP-01 §6), then the socket goes.
fn stop(state: &mut AbyssState) {
    if let Some(t) = state.agents.listener.take() {
        state.loop_handle.remove(t);
    }
    if let Some(dh) = state.agents.display.clone() {
        if let Some(g) = state.agents.global.take() {
            dh.remove_global::<AbyssState>(g);
        }
        let backend = dh.backend_handle();
        for id in state.agents.clients.drain(..) {
            backend.kill_client(id, DisconnectReason::ConnectionClosed);
        }
    }
    state.agents.slots.clear();
    cleanup(state);
    state.agents.path = None;
    tracing::info!("agents hook off; agent socket removed");
}

/// Unlink the socket on a clean shutdown.
pub fn cleanup(state: &AbyssState) {
    if let Some(path) = &state.agents.path {
        let _ = std::fs::remove_file(path);
    }
}

/// The COMP-13 §3 door check: the kernel's record of the peer, never
/// anything it sent.
fn peer_is_us(stream: &UnixStream) -> bool {
    rustix::net::sockopt::socket_peercred(stream).is_ok_and(|c| c.uid == rustix::process::getuid())
}

fn accept(state: &mut AbyssState, stream: UnixStream) {
    if !peer_is_us(&stream) {
        tracing::warn!("agent socket peer is not the session uid; refused");
        return;
    }
    // Only `start` registers the listener, after the display exists.
    let Some(mut dh) = state.agents.display.clone() else {
        return;
    };
    let data = Arc::new(ClientState {
        agent: true,
        ..Default::default()
    });
    // Forget clients that have gone, which bounds the list by live ones.
    let backend = dh.backend_handle();
    state
        .agents
        .clients
        .retain(|id| backend.get_client_data(id.clone()).is_ok());
    match dh.insert_client(stream, data) {
        Ok(client) => {
            state.agents.clients.push(client.id());
            tracing::info!("agent client connected");
        }
        Err(e) => tracing::warn!(%e, "inserting an agent client"),
    }
}

fn is_agent_client(client: &Client) -> bool {
    client.get_data::<ClientState>().is_some_and(|d| d.agent)
}

// ------------------------------------------------------------ manager

impl GlobalDispatch<EclipseAgentManagerV1, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<EclipseAgentManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        manager.dedupe_window(DEDUPE_WINDOW_S);
    }

    /// C-00 §8.1: connections on the normal socket never see these globals.
    /// wayland-backend applies this to `bind` as well as to the listing.
    fn can_view(client: Client, _global_data: &()) -> bool {
        is_agent_client(&client)
    }
}

impl Dispatch<EclipseAgentManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &EclipseAgentManagerV1,
        request: eclipse_agent_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use eclipse_agent_manager_v1::{Error, Request};
        let Request::CreateAgent { id, grant } = request else {
            return;
        };
        // policyd revocation push (main thread)
        match Agent::admit(&grant, state.policy_key.as_ref(), now_ms(), |_| false) {
            Ok(agent) => {
                state.agents.next_id += 1;
                let aid = state.agents.next_id;
                tracing::info!(agent = aid, principal = agent.principal(), "agent admitted");
                audit::agent(state, audit::lifecycle(agent.principal(), "start"));
                state.agents.slots.push((aid, Some(agent)));
                data_init.init(id, AgentId(aid));
            }
            Err(e) => {
                // The new object has to exist before its creator's error.
                data_init.init(id, AgentId(0));
                let (code, msg) = refusal(e);
                tracing::warn!(reason = msg, "create_agent refused");
                manager.post_error(
                    match code {
                        Refusal::PolicyUnavailable => Error::PolicyUnavailable,
                        Refusal::InvalidGrant => Error::InvalidGrant,
                    },
                    msg,
                );
            }
        }
    }
}

enum Refusal {
    PolicyUnavailable,
    InvalidGrant,
}

/// COMP-08 §1 and COMP-01 §6: no key is `POLICY_UNAVAILABLE`; every other
/// failure is `INVALID_GRANT`. The message names the class of failure only.
fn refusal(e: AdmitError) -> (Refusal, &'static str) {
    match e {
        AdmitError::PolicyUnavailable => (Refusal::PolicyUnavailable, "policyd is not connected"),
        AdmitError::Invalid(_) => (Refusal::InvalidGrant, "grant failed verification"),
        AdmitError::WrongPrincipal => (Refusal::InvalidGrant, "grant principal refused"),
        AdmitError::Revoked => (Refusal::InvalidGrant, "grant revoked"),
    }
}

// ------------------------------------------------------------ agent

impl Dispatch<EclipseAgentV1, AgentId> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        agent: &EclipseAgentV1,
        request: eclipse_agent_v1::Request,
        data: &AgentId,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use eclipse_agent_v1::{Error, Request};
        match request {
            Request::GetScene { id } => {
                data_init.init(id, *data);
            }
            Request::AddGrant { grant } => {
                let Some(mut a) = state.agents.take(data.0) else {
                    agent.post_error(Error::InvalidGrant, "no such agent");
                    return;
                };
                // policyd revocation push (main thread)
                let r = a.add_grant(&grant, state.policy_key.as_ref(), now_ms(), |_| false);
                state.agents.put(data.0, a);
                if let Err(e) = r {
                    let (code, msg) = refusal(e);
                    tracing::warn!(agent = data.0, reason = msg, "add_grant refused");
                    agent.post_error(
                        match code {
                            Refusal::PolicyUnavailable => Error::PolicyUnavailable,
                            Refusal::InvalidGrant => Error::InvalidGrant,
                        },
                        msg,
                    );
                }
            }
            Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _resource: &EclipseAgentV1, data: &AgentId) {
        if let Some(principal) = state
            .agents
            .slots
            .iter()
            .find(|(i, _)| *i == data.0)
            .and_then(|(_, a)| a.as_ref())
            .map(|a| a.principal().to_owned())
        {
            audit::agent(state, audit::lifecycle(&principal, "stop"));
        }
        state.agents.slots.retain(|(i, _)| *i != data.0);
    }
}

// ------------------------------------------------------------ scene

/// One `result` with no focus, generation or latency yet (COMP-08 §2).
fn result(scene: &EclipseSceneV1, req_id: u32, status: Status, detail: &str) {
    scene.result(req_id, status as u32, detail.to_owned(), 0, 0, 0);
}

impl Dispatch<EclipseSceneV1, AgentId> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        scene: &EclipseSceneV1,
        request: eclipse_scene_v1::Request,
        data: &AgentId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use eclipse_scene_v1::Request;
        let req_id = match &request {
            Request::ListToplevels { req_id, .. }
            | Request::GetToplevel { req_id, .. }
            | Request::HitTest { req_id, .. } => *req_id,
            _ => return,
        };
        // F-08: while policyd is down every agent is paused. Nothing is
        // looked at, nothing changes, not even grant expiry.
        if state.policy_key.is_none() {
            result(scene, req_id, Status::Paused, "");
            return;
        }
        let Some(mut agent) = state.agents.take(data.0) else {
            result(
                scene,
                req_id,
                Status::NoCapability,
                policy_eval::scope::SCENE_LIST,
            );
            return;
        };
        let started = std::time::Instant::now();
        let task = agent.task_id();
        // COMP-12 §1: no request runs before its record is in the socket.
        let (name, args) = describe(&request);
        let req = audit::request(agent.principal(), task, req_id, "eclipse_scene_v1", name, args);
        if !audit::begin(state, req) {
            state.agents.put(data.0, agent);
            result(scene, req_id, Status::Paused, "");
            return;
        }
        let out = answer(state, scene, &mut agent, request);
        let us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        audit::agent(
            state,
            audit::decision(agent.principal(), task, req_id, out.allow, out.rule, "scope", us),
        );
        audit::agent(
            state,
            audit::result(agent.principal(), req_id, out.status as u32, out.detail, us),
        );
        state.agents.put(data.0, agent);
    }
}

fn describe(request: &eclipse_scene_v1::Request) -> (&'static str, audit::Args<'_>) {
    use eclipse_scene_v1::Request;
    match request {
        Request::ListToplevels { filter, .. } => ("list_toplevels", audit::Args::Filter(filter)),
        Request::GetToplevel { handle, .. } => ("get_toplevel", audit::Args::Handle(u64::from(*handle))),
        Request::HitTest { x, y, .. } => ("hit_test", audit::Args::Point(i64::from(*x), i64::from(*y))),
        _ => ("unknown", audit::Args::None),
    }
}

/// How a scene request ended, for its `decision` and `result` records.
struct Outcome {
    /// Whether the capability check passed. A malformed argument can fail a
    /// request the check allowed.
    allow: bool,
    status: Status,
    /// The capability the request was checked against.
    rule: &'static str,
    /// The `result` detail the agent was sent.
    detail: &'static str,
}

impl Outcome {
    fn ok(rule: &'static str) -> Self {
        Outcome {
            allow: true,
            status: Status::Ok,
            rule,
            detail: "",
        }
    }

    fn refused(
        scene: &EclipseSceneV1,
        req_id: u32,
        status: Status,
        rule: &'static str,
        detail: &'static str,
    ) -> Self {
        result(scene, req_id, status, detail);
        Outcome {
            allow: false,
            status,
            rule,
            detail,
        }
    }

    /// Allowed by the check, refused for a malformed argument.
    fn invalid(scene: &EclipseSceneV1, req_id: u32, rule: &'static str, detail: &'static str) -> Self {
        Outcome {
            allow: true,
            ..Outcome::refused(scene, req_id, Status::InvalidArgument, rule, detail)
        }
    }

    /// A request this compositor does not know: denied, nothing done, and
    /// no result to send (fail-closed).
    fn unknown() -> Self {
        Outcome {
            allow: false,
            status: Status::InvalidArgument,
            rule: "unknown",
            detail: "request",
        }
    }
}

fn answer(
    state: &mut AbyssState,
    scene: &EclipseSceneV1,
    agent: &mut Agent,
    request: eclipse_scene_v1::Request,
) -> Outcome {
    use eclipse_scene_v1::Request;
    use policy_eval::scope::SCENE_LIST;
    let Some(views) = agent.view_at(now_ms()) else {
        let req_id = match request {
            Request::ListToplevels { req_id, .. }
            | Request::GetToplevel { req_id, .. }
            | Request::HitTest { req_id, .. } => req_id,
            _ => return Outcome::unknown(),
        };
        return Outcome::refused(scene, req_id, Status::NoCapability, SCENE_LIST, SCENE_LIST);
    };
    let view = views.list;
    match request {
        Request::ListToplevels { req_id, filter } => {
            // TODO(M11+): the S-01 §3 KDL filter grammar. Until it is parsed,
            // only "no filter" has a meaning; anything else is refused rather
            // than silently ignored.
            if !filter.is_empty() {
                return Outcome::invalid(scene, req_id, SCENE_LIST, "filter");
            }
            for (handle, window) in scene::list(state, view) {
                // A handle the wire cannot carry is a window the agent
                // cannot see, never a truncated one.
                let Ok(handle) = u32::try_from(handle) else {
                    continue;
                };
                send_toplevel(state, scene, req_id, handle, &window);
            }
            scene.toplevels_done(req_id);
            Outcome::ok(SCENE_LIST)
        }
        Request::GetToplevel { req_id, handle } => {
            // F-07: unknown, gone, out of scope and no-agent are one answer,
            // reached by one path.
            let Some(window) = scene::resolve(state, view, u64::from(handle)) else {
                return Outcome::refused(scene, req_id, Status::InvalidArgument, SCENE_LIST, "handle");
            };
            if !scene::readable(state, views.read, &window) {
                return Outcome::refused(scene, req_id, unreadable(views), SCENE_READ, SCENE_READ);
            }
            send_detail(state, scene, req_id, handle, &window);
            Outcome::ok(SCENE_READ)
        }
        Request::HitTest { req_id, x, y } => {
            let pos = (f64::from(x), f64::from(y)).into();
            // COMP-08 §3: nothing visible on top is handle 0, and so is a
            // window whose handle the wire cannot carry.
            let hit = scene::hit(state, view, pos).and_then(|(window, local)| {
                let handle = u32::try_from(state.ipc.handle_for(&window)).ok()?;
                Some((window, local, handle))
            });
            let Some((window, local, handle)) = hit else {
                scene.hit(req_id, 0, 0, 0, 0, 0);
                return Outcome::ok(SCENE_LIST);
            };
            if !scene::readable(state, views.read, &window) {
                return Outcome::refused(scene, req_id, unreadable(views), SCENE_READ, SCENE_READ);
            }
            scene.hit(req_id, handle, local.x as i32, local.y as i32, 0, 0);
            Outcome::ok(SCENE_READ)
        }
        _ => Outcome::unknown(),
    }
}

/// A visible window `scene.read` does not reach: out of scope when a grant
/// holds `scene.read` at all, otherwise the capability is missing (S-01 §3,
/// COMP-08 §2.1).
fn unreadable(views: Views<'_>) -> Status {
    if views.read_held {
        Status::OutOfScope
    } else {
        Status::NoCapability
    }
}

/// `eclipse_scene_v1.sensitivity` for a window the agent can see.
fn sensitivity(state: &AbyssState, window: &Window) -> u32 {
    match scene::class(state, window) {
        Class::Public => 0,
        Class::Private => 1,
        Class::Secret => 2,
    }
}

/// What a `toplevel` event carries.
struct Facts {
    handle: u32,
    app_id: String,
    title: String,
    pid: u32,
    workspace: u32,
    output: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: f64,
    states: u32,
}

fn identity(window: &Window) -> (String, String) {
    if let Some(x) = window.x11_surface() {
        return (x.class(), x.title());
    }
    let (a, t) = crate::ipc::methods::identity_of(window);
    (a.unwrap_or_default(), t.unwrap_or_default())
}

fn facts(state: &AbyssState, handle: u32, window: &Window) -> Facts {
    use eclipse_scene_v1::State as S;
    let (app_id, title) = identity(window);
    let (output, workspace) = crate::ipc::methods::location_of(state, window).unwrap_or((0, 0));
    let geo = state.space.element_geometry(window).unwrap_or_default();
    let scale = state
        .outputs
        .get(output)
        .map(|e| e.output.current_scale().fractional_scale())
        .unwrap_or(1.0);
    let (maximized, fullscreen) = match (window.toplevel(), window.x11_surface()) {
        (Some(t), _) => {
            use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as X;
            let s = t.current_state().states;
            (s.contains(X::Maximized), s.contains(X::Fullscreen))
        }
        (None, Some(x)) => (x.is_maximized(), x.is_fullscreen()),
        _ => (false, false),
    };
    let mut states = 0;
    for (on, bit) in [
        (maximized, S::Maximized),
        (fullscreen, S::Fullscreen),
        (state.focus.as_ref() == Some(window), S::Focused),
        (crate::ipc::methods::is_floating(state, window), S::Floating),
        (crate::shell::is_minimized(state, window), S::Minimized),
    ] {
        if on {
            states |= bit as u32;
        }
    }
    Facts {
        handle,
        app_id,
        title,
        pid: crate::ipc::methods::pid_of(state, window).map_or(0, |p| p as u32),
        workspace: workspace as u32,
        output: output as u32,
        x: geo.loc.x,
        y: geo.loc.y,
        w: geo.size.w,
        h: geo.size.h,
        scale,
        states,
    }
}

fn send_toplevel(state: &AbyssState, scene: &EclipseSceneV1, req_id: u32, handle: u32, window: &Window) {
    let t = facts(state, handle, window);
    scene.toplevel(
        req_id,
        t.handle,
        t.app_id,
        t.title,
        t.pid,
        t.workspace,
        t.output,
        t.x,
        t.y,
        t.w,
        t.h,
        t.scale,
        t.states,
        sensitivity(state, window),
        0,
        0,
        0,
        0,
        0,
    );
}

/// `get_toplevel`'s answer: the listing's `toplevel`, then what only
/// `scene.read` adds. One event cannot carry both (libwayland caps an event
/// at 20 arguments).
fn send_detail(state: &AbyssState, scene: &EclipseSceneV1, req_id: u32, handle: u32, window: &Window) {
    send_toplevel(state, scene, req_id, handle, window);
    // Server-side decorations are not drawn yet, so the decorated rect is the
    // window's own. Popups and per-seat focus arrive with their milestones.
    let geo = state.space.element_geometry(window).unwrap_or_default();
    scene.toplevel_detail(
        req_id,
        handle,
        geo.loc.x,
        geo.loc.y,
        geo.size.w,
        geo.size.h,
        Vec::new(),
        Vec::new(),
    );
}
