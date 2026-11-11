// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_session_lock_v1` (COMP-06, COMP-10 §5).
//!
//! The lock is compositor-side and unbypassable: while [`LockState::locked`] is
//! set, each backend draws [`lock_elements`] and nothing else: the locker's
//! surface for that output, or a compositor-drawn fallback where the locker
//! has not provided one. A locker that dies keeps the session locked behind that
//! fallback (ADR 0024) — a crash must never be a way in.
//!
//! One lock object holds the session at a time (ADR 0099). A second locker
//! while that one is alive is sent `finished`, and only the holder's
//! `unlock_and_destroy` unlocks. If the holder dies, the session stays locked,
//! `idle.lock-command` is relaunched with backoff, and a locker running the
//! lock command's own program may take the ownerless session over without it
//! ever unlocking. Any other client is sent `finished`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::render_elements_from_surface_tree,
            Kind,
        },
        gles::GlesRenderer,
    },
    input::pointer::MotionEvent,
    output::Output,
    reexports::{
        wayland_protocols::ext::session_lock::v1::server::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
        wayland_server::{
            backend::ClientId,
            delegate_dispatch, delegate_global_dispatch,
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, Resource,
        },
    },
    utils::{Logical, Point, Scale, Size, SERIAL_COUNTER},
    wayland::{
        compositor::get_parent,
        session_lock::{
            ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
            SessionLockManagerState, SessionLockState, SessionLocker,
        },
    },
};

use crate::{render::AbyssRenderElement, state::AbyssState};

/// Fallback screen: pure black with a centred amber bar (eclipse trusted-UI
/// palette). Compositor-drawn — never a client surface.
const FALLBACK_BG: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const FALLBACK_FG: [f32; 4] = [1.0, 0.72, 0.15, 1.0];
const FALLBACK_BAR_H: i32 = 4;
const FALLBACK_BAR_W: i32 = 220;

/// Locker relaunch backoff (ADR 0099): the wait before the first relaunch
/// after the locker dies, then between relaunches that did not take over. Its
/// length is the cap: after that many relaunches in one locked session the
/// compositor stops, so a locker that crashes on start cannot fork-bomb.
const RELAUNCH_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(8),
];

/// How long a spawned lock command counts as `pending` before it is taken not
/// to have come up. Also the lock-before-suspend wait (`input::idle`).
pub const LOCK_PENDING: Duration = Duration::from_secs(10);

/// Relaunch bookkeeping for one locked session; reset by `unlock`.
#[derive(Default)]
struct Relaunch {
    /// Relaunches already spawned this locked session.
    attempts: usize,
    /// When to relaunch next, if the session is still ownerless then.
    next: Option<Instant>,
}

/// Lock surfaces and fallback quads, kept between frames.
#[derive(Default)]
pub struct LockState {
    pub locked: bool,
    surfaces: Vec<(Output, LockSurface)>,
    /// Per-output (background, bar) quads, keyed by output name.
    fallback: HashMap<String, (SolidColorBuffer, SolidColorBuffer)>,
    /// The lock object holding the session. `None` while unlocked, and while
    /// locked after the locker died (the fallback is showing).
    owner: Option<ExtSessionLockV1>,
    /// When the compositor last ran the lock command and no locker has locked
    /// since. Feeds `pending`.
    requested: Option<Instant>,
    relaunch: Relaunch,
}

impl LockState {
    /// A live locker holds the session.
    pub fn locker_alive(&self) -> bool {
        self.owner.as_ref().is_some_and(|l| l.is_alive())
    }

    fn is_owner(&self, lock: &ExtSessionLockV1) -> bool {
        self.owner.as_ref() == Some(lock)
    }

    /// A locker has been asked for (the lock command ran, or a relaunch is
    /// scheduled) and none holds the session yet.
    pub fn pending(&self, now: Instant) -> bool {
        !self.locker_alive()
            && (self.relaunch.next.is_some()
                || self
                    .requested
                    .is_some_and(|t| now.saturating_duration_since(t) < LOCK_PENDING))
    }

    /// Schedule the next relaunch from `now`. `None` once the cap is reached,
    /// and then nothing is scheduled.
    fn schedule_relaunch(&mut self, now: Instant) -> Option<Duration> {
        let wait = RELAUNCH_BACKOFF.get(self.relaunch.attempts).copied();
        self.relaunch.next = wait.map(|w| now + w);
        wait
    }

    /// Whether a relaunch is due at `now`. Consumes it and schedules the next
    /// check, so a relaunch that never takes over is followed by another.
    fn poll_relaunch(&mut self, now: Instant) -> bool {
        let Some(at) = self.relaunch.next else {
            return false;
        };
        if !self.locked || self.locker_alive() {
            self.relaunch.next = None;
            return false;
        }
        if now < at {
            return false;
        }
        self.relaunch.attempts += 1;
        self.schedule_relaunch(now);
        true
    }

    /// The locker's surface for `output`, if it supplied one and it is alive.
    pub fn surface_for(&self, output: &Output) -> Option<&LockSurface> {
        self.surfaces
            .iter()
            .find(|(o, s)| o == output && s.alive())
            .map(|(_, s)| s)
    }

    /// The compositor-drawn fallback quads for `output`, sized to it.
    pub fn fallback_for(&mut self, output: &Output) -> &(SolidColorBuffer, SolidColorBuffer) {
        let size = logical_size(output);
        let bar = Size::<i32, Logical>::from((FALLBACK_BAR_W.min(size.w), FALLBACK_BAR_H));
        let entry = self.fallback.entry(output.name()).or_insert_with(|| {
            (
                SolidColorBuffer::new(size, FALLBACK_BG),
                SolidColorBuffer::new(bar, FALLBACK_FG),
            )
        });
        entry.0.update(size, FALLBACK_BG);
        entry.1.update(bar, FALLBACK_FG);
        entry
    }

    /// Whether `surface` is one of the locker's surfaces or a subsurface of
    /// one. While locked, nothing else may hold keyboard focus (COMP-06 §1, COMP-04 §7).
    pub fn owns(&self, surface: &WlSurface) -> bool {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        self.surfaces.iter().any(|(_, s)| s.wl_surface() == &root)
    }

    /// Drop state for an output that went away.
    pub fn forget_output(&mut self, output: &Output) {
        self.surfaces.retain(|(o, _)| o != output);
        self.fallback.remove(&output.name());
    }
}

/// Logical size of `output` at its current mode, scale and transform.
pub fn logical_size(output: &Output) -> Size<i32, Logical> {
    let Some(mode) = output.current_mode() else {
        return (0, 0).into();
    };
    output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(output.current_scale().fractional_scale())
        .to_i32_round()
}

impl AbyssState {
    /// Everything `lock` does before confirming. Split out because smithay's
    /// `SessionLocker` cannot be built outside a real client, and this is the
    /// half that has to be tested.
    pub(crate) fn engage_lock(&mut self) {
        self.lock.locked = true;
        tracing::info!("session locked");
        // S-08 §2: the secret store locks with the screen.
        crate::trusted_ui::broker::session_locked(self);
        // A window mid-drag goes back to its tile rather than landing blind.
        crate::shell::cancel_tile_drag(self);
        self.gesture_drag = None;
        // A scripted drag waiting on its release lets go now.
        self.cancel_drag_hold();
        // Nothing behind the lock may keep focus, even for one frame.
        if let Some(keyboard) = self.seat.get_keyboard() {
            // An input-method grab receives every key whatever holds focus,
            // so it would receive the unlock password.
            keyboard.unset_grab(self);
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
        crate::audit::focus(self, None, "human");
        // The window menu and the resize cursor belong to the session behind
        // the lock.
        self.winmenu.close();
        crate::overview::close_all(self);
        crate::shell::edge::restore_cursor(self);
        // The region selector is a modal over the session behind the lock.
        self.region_select.cancel();
        if let Some(pointer) = self.seat.get_pointer() {
            let time = self.start_time.elapsed().as_millis() as u32;
            // A button held across the lock leaves a click grab pinning pointer
            // focus to the app, and a grab ignores the focus `motion` passes it.
            // Clearing the pending focus first means ending the grab restores
            // to nothing rather than back to the app.
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location: self.pointer_location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
            pointer.frame(self);
        }
        self.last_pointer_focus = None;
        // Touch too: smithay's touch grab keeps a point bound to the surface
        // it came down on, so a finger already down would go on driving the
        // app behind the lock. Cancel the sequence outright.
        self.on_touch_cancel();
        crate::backend::damage_all(self);
    }
}

impl SessionLockHandler for AbyssState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock.locker_alive() {
            // ext-session-lock-v1: a lock while another is held is refused
            // with `finished`, which dropping the confirmation sends. The held
            // lock is not touched.
            tracing::info!("a second locker asked while the session is held, sent finished");
            return;
        }
        let takeover = self.lock.locked;
        if takeover && !may_take_over(self, confirmation.ext_session_lock()) {
            // Ownerless and locked: only the configured locker's program may
            // take it over (ADR 0099). Everyone else, and everyone when no
            // lock-command is set, leaves it on the fallback (ADR 0024).
            tracing::warn!(
                "a lock on the ownerless session from a client that is not the lock command, sent finished"
            );
            return;
        }
        self.lock.owner = Some(confirmation.ext_session_lock().clone());
        self.lock.requested = None;
        self.lock.relaunch.next = None;
        if takeover {
            // Locked behind the fallback since the old locker died: nothing
            // behind the lock is reachable, so there is nothing to engage
            // again. `locked` never went false, so no frame was unlocked.
            self.lock.surfaces.retain(|(_, s)| s.alive());
            if let Some(keyboard) = self.seat.get_keyboard() {
                // Clear the dead locker's focus so the new one's first
                // surface takes the keyboard (`new_surface`).
                keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
            }
            tracing::info!("a replacement locker took over the locked session");
        } else {
            self.engage_lock();
        }
        confirmation.lock();
        emit_lock_state(self);
    }

    /// Reached only from the holding lock's `unlock_and_destroy`: the
    /// `Dispatch` below refuses it from any other lock object, because smithay
    /// calls this with no word of which lock asked.
    fn unlock(&mut self) {
        self.lock.locked = false;
        self.lock.owner = None;
        self.lock.requested = None;
        self.lock.relaunch = Relaunch::default();
        self.lock.surfaces.clear();
        tracing::info!("session unlocked");
        crate::shell::refocus_topmost(self);
        crate::backend::damage_all(self);
        crate::trusted_ui::broker::session_unlocked(self);
        emit_lock_state(self);
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        // A refused locker can still ask for a lock surface on its finished
        // lock; smithay creates it. It is never kept, so never drawn.
        let owner = self.lock.owner.as_ref().and_then(|l| l.client()).map(|c| c.id());
        let from = surface.wl_surface().client().map(|c| c.id());
        if owner.is_none() || owner != from {
            tracing::warn!("lock surface from a client that does not hold the lock, ignored");
            return;
        }
        let Some(output) = Output::from_resource(&output) else {
            tracing::warn!("lock surface for an unknown output");
            return;
        };
        let size = logical_size(&output);
        surface.with_pending_state(|state| {
            state.size = Some((size.w.max(0) as u32, size.h.max(0) as u32).into());
        });
        surface.send_configure();
        self.lock.surfaces.retain(|(o, _)| o != &output);
        let wl = surface.wl_surface().clone();
        self.lock.surfaces.push((output, surface));
        // Input goes to the locker and nowhere else.
        if let Some(keyboard) = self.seat.get_keyboard() {
            if keyboard.current_focus().is_none() {
                keyboard.set_focus(self, Some(wl.into()), SERIAL_COUNTER.next_serial());
            }
        }
        crate::backend::damage_all(self);
    }
}

// `delegate_session_lock!` minus the lock object, which is dispatched by hand
// below so that it can see which lock is asking.
delegate_global_dispatch!(AbyssState: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState);
delegate_dispatch!(AbyssState: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
delegate_dispatch!(AbyssState: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState);

impl Dispatch<ExtSessionLockV1, SessionLockState> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        // smithay 0.7.0 posts `invalid_unlock` for a lock that never locked
        // but calls `unlock` anyway, and `unlock` cannot tell who asked. Only
        // the holder may unlock; anyone else is a protocol error and nothing
        // else happens.
        if matches!(request, ext_session_lock_v1::Request::UnlockAndDestroy) && !state.lock.is_owner(resource)
        {
            tracing::warn!("unlock from a lock that does not hold the session, refused");
            resource.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "this lock does not hold the session",
            );
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            state, client, resource, request, data, dhandle, data_init,
        );
    }

    fn destroyed(state: &mut Self, client: ClientId, resource: &ExtSessionLockV1, data: &SessionLockState) {
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::destroyed(
            state, client, resource, data,
        );
        lock_object_gone(state, resource);
    }
}

/// A lock object went away: a destructor request, or its client's death. If it
/// held a locked session, the session stays locked (ADR 0024) and the lock
/// command is relaunched (ADR 0099). After a legitimate `unlock_and_destroy`
/// `owner` is already `None` and this does nothing.
fn lock_object_gone(state: &mut AbyssState, lock: &ExtSessionLockV1) {
    if !state.lock.is_owner(lock) {
        return;
    }
    state.lock.owner = None;
    if !state.lock.locked {
        return;
    }
    tracing::warn!("the locker went away without unlocking, the session stays locked");
    if state.config.idle.lock_command.is_some() && !state.kiosk {
        match state.lock.schedule_relaunch(Instant::now()) {
            Some(wait) => tracing::info!(wait_ms = wait.as_millis() as u64, "relaunching the lock command"),
            None => tracing::error!(
                "locker relaunch limit reached, the session stays locked behind the fallback (`ec-ctl lock` retries)"
            ),
        }
    }
    crate::backend::damage_all(state);
    emit_lock_state(state);
}

/// The program `idle.lock-command` runs, as a resolved path (ADR 0099): its
/// first word, an absolute path as itself, a bare name as `/usr/bin/<name>`
/// (never a `PATH` search), symlinks resolved. `None` for anything else — a
/// relative path, shell syntax, a quote, an env assignment, a path that does
/// not resolve — and `None` means no takeover.
pub(crate) fn locker_program(cmd: &str) -> Option<PathBuf> {
    let argv0 = cmd.split_whitespace().next()?;
    if argv0.chars().any(|c| "$`'\"\\;|&<>(){}[]*?~=!#".contains(c)) {
        return None;
    }
    let path = if argv0.starts_with('/') {
        PathBuf::from(argv0)
    } else if argv0.contains('/') {
        return None;
    } else {
        PathBuf::from("/usr/bin").join(argv0)
    };
    std::fs::canonicalize(path).ok()
}

/// Whether the client behind `lock` runs the lock command's program: the
/// kernel's `/proc/<pid>/exe` for the `SO_PEERCRED` pid, compared as a path.
/// Fail-closed: no lock-command, no credentials, an unreadable or deleted
/// executable all answer false. Path identity only, with ADR 0094's limits:
/// anything that can run that binary as this user passes.
fn may_take_over(state: &AbyssState, lock: &ExtSessionLockV1) -> bool {
    let Some(program) = state.config.idle.lock_command.as_deref().and_then(locker_program) else {
        return false;
    };
    let Some(client) = lock.client() else {
        return false;
    };
    let Ok(creds) = client.get_credentials(&state.display_handle) else {
        return false;
    };
    if creds.pid <= 0 {
        return false;
    }
    std::fs::read_link(format!("/proc/{}/exe", creds.pid)).is_ok_and(|exe| exe == program)
}

/// `{locked, pending, locker_alive}`: the `get_lock_state` reply and the `lock`
/// event payload.
pub(crate) fn lock_payload(state: &AbyssState) -> Value {
    json!({
        "locked": state.lock.locked,
        "pending": state.lock.pending(Instant::now()),
        "locker_alive": state.lock.locker_alive(),
    })
}

fn emit_lock_state(state: &mut AbyssState) {
    let payload = lock_payload(state);
    crate::ipc::emit(state, "lock", payload);
}

/// Run `idle.lock-command`: the one door for every locker the compositor
/// starts (idle timeout, `lock_session`, relaunch, lock-before-suspend), all
/// through `shell::spawn`. False if there is no command, or in kiosk mode
/// (the greeter), where nobody is logged in to lock.
pub(crate) fn spawn_locker(state: &mut AbyssState, why: &'static str) -> bool {
    if state.kiosk {
        return false;
    }
    let Some(cmd) = state.config.idle.lock_command.clone() else {
        return false;
    };
    tracing::info!(why, "spawning locker");
    crate::shell::spawn(state, &cmd);
    state.lock.requested = Some(Instant::now());
    emit_lock_state(state);
    true
}

/// Once a second, from the idle clock (`input::idle`): relaunch the locker if
/// one is due. Runs whatever the idle inhibitors say.
pub(crate) fn tick(state: &mut AbyssState, now: Instant) {
    if !state.lock.poll_relaunch(now) {
        return;
    }
    let attempt = state.lock.relaunch.attempts;
    tracing::warn!(
        attempt,
        "session locked with no locker, relaunching the lock command"
    );
    spawn_locker(state, "relaunch");
    if state.lock.relaunch.next.is_none() {
        tracing::error!(
            attempt,
            "last locker relaunch, the session stays locked behind the fallback if it does not come up"
        );
    }
}

/// Everything drawn on `output` while the session is locked: the locker's
/// surface if it gave us one, otherwise the compositor-drawn fallback. No
/// client content of any other kind is ever included here.
pub fn lock_elements(
    renderer: &mut GlesRenderer,
    lock: &mut LockState,
    output: &Output,
) -> Vec<AbyssRenderElement> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    if let Some(surface) = lock.surface_for(output) {
        return render_elements_from_surface_tree(
            renderer,
            surface.wl_surface(),
            (0, 0),
            scale,
            1.0,
            Kind::Unspecified,
        );
    }

    let size = logical_size(output);
    let (bg, bar) = lock.fallback_for(output);
    let bar_w = FALLBACK_BAR_W.min(size.w);
    let bar_loc = Point::<i32, Logical>::from(((size.w - bar_w) / 2, (size.h - FALLBACK_BAR_H) / 2));
    vec![
        AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
            bar,
            bar_loc.to_f64().to_physical_precise_round(scale),
            scale,
            1.0,
            Kind::Unspecified,
        )),
        AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
            bg,
            (0, 0),
            scale,
            1.0,
            Kind::Unspecified,
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::focus::state_tests::{harness, Harness};
    use smithay::reexports::wayland_server::backend::DisconnectReason;
    use std::os::unix::net::UnixStream;
    use wayland_client::{
        delegate_noop,
        protocol::{
            wl_compositor::WlCompositor, wl_output::WlOutput as CWlOutput, wl_registry,
            wl_surface::WlSurface as CWlSurface,
        },
        Connection, Dispatch as CDispatch, EventQueue, QueueHandle,
    };
    use wayland_protocols::ext::session_lock::v1::client::{
        ext_session_lock_manager_v1::ExtSessionLockManagerV1 as CManager,
        ext_session_lock_surface_v1::ExtSessionLockSurfaceV1 as CLockSurface,
        ext_session_lock_v1::{self as clock, ExtSessionLockV1 as CLock},
    };

    #[derive(Default)]
    struct Seen {
        globals: Vec<(u32, String, u32)>,
        locked: u32,
        finished: u32,
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

    impl CDispatch<CLock, ()> for Seen {
        fn event(s: &mut Self, _: &CLock, e: clock::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match e {
                clock::Event::Locked => s.locked += 1,
                clock::Event::Finished => s.finished += 1,
                _ => {}
            }
        }
    }

    delegate_noop!(Seen: CManager);
    delegate_noop!(Seen: WlCompositor);
    delegate_noop!(Seen: ignore CWlSurface);
    delegate_noop!(Seen: ignore CWlOutput);
    delegate_noop!(Seen: ignore CLockSurface);

    struct Client {
        server: smithay::reexports::wayland_server::Client,
        conn: Connection,
        queue: EventQueue<Seen>,
        seen: Seen,
        compositor: WlCompositor,
        manager: CManager,
        output: CWlOutput,
    }

    impl Client {
        fn connect(h: &mut Harness) -> Self {
            let (server, client) = UnixStream::pair().unwrap();
            let server = h
                .state
                .display_handle
                .insert_client(server, crate::state::client_state())
                .unwrap();
            let conn = Connection::from_socket(client).unwrap();
            let mut queue = conn.new_event_queue();
            let qh = queue.handle();
            let registry = conn.display().get_registry(&qh, ());
            let mut seen = Seen::default();
            for _ in 0..4 {
                let _ = conn.flush();
                h.dispatch();
                if let Some(g) = conn.prepare_read() {
                    let _ = g.read();
                }
                let _ = queue.dispatch_pending(&mut seen);
            }
            let find = |name: &str| {
                seen.globals
                    .iter()
                    .find(|(_, i, _)| i == name)
                    .map(|(n, _, v)| (*n, *v))
                    .unwrap_or_else(|| panic!("{name} advertised"))
            };
            let (n, v) = find("wl_compositor");
            let compositor = registry.bind(n, v.min(5), &qh, ());
            let (n, _) = find("ext_session_lock_manager_v1");
            let manager = registry.bind(n, 1, &qh, ());
            let (n, _) = find("wl_output");
            let output = registry.bind(n, 1, &qh, ());
            let mut c = Self {
                server,
                conn,
                queue,
                seen,
                compositor,
                manager,
                output,
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

        fn lock(&mut self, h: &mut Harness) -> CLock {
            let lock = self.manager.lock(&self.queue.handle(), ());
            self.pump(h);
            lock
        }

        /// A lock surface on the first output. No buffer: `new_surface` is
        /// what is under test, not drawing.
        fn lock_surface(&mut self, h: &mut Harness, lock: &CLock) {
            let qh = self.queue.handle();
            let surface = self.compositor.create_surface(&qh, ());
            let _ = lock.get_lock_surface(&surface, &self.output, &qh, ());
            self.pump(h);
        }

        fn error(&self) -> Option<u32> {
            self.conn.protocol_error().map(|e| e.code)
        }

        fn holds(&self, h: &Harness) -> bool {
            h.state
                .lock
                .owner
                .as_ref()
                .and_then(|l| l.client())
                .map(|c| c.id())
                == Some(self.server.id())
        }
    }

    /// Item 1 of ADR 0099: a second locker while one holds the session is
    /// sent `finished`, cannot attach a surface, cannot unlock, and the held
    /// lock is undisturbed.
    #[test]
    fn a_second_locker_is_finished_and_cannot_unlock() {
        let mut h = harness();
        let mut a = Client::connect(&mut h);
        let mut b = Client::connect(&mut h);
        let lock_a = a.lock(&mut h);
        assert_eq!((a.seen.locked, a.seen.finished), (1, 0));
        assert!(h.state.lock.locked && a.holds(&h));

        let lock_b = b.lock(&mut h);
        assert_eq!(
            (b.seen.locked, b.seen.finished),
            (0, 1),
            "second locker is finished"
        );
        assert!(h.state.lock.locked && a.holds(&h), "the held lock is undisturbed");

        b.lock_surface(&mut h, &lock_b);
        assert!(h.state.lock.surfaces.is_empty(), "a refused locker draws nothing");
        a.lock_surface(&mut h, &lock_a);
        assert_eq!(h.state.lock.surfaces.len(), 1);

        lock_b.unlock_and_destroy();
        b.pump(&mut h);
        assert_eq!(
            b.error(),
            Some(u32::from(clock::Error::InvalidUnlock)),
            "unlock from a lock that does not hold the session is a protocol error"
        );
        assert!(h.state.lock.locked && h.state.lock.locker_alive() && a.holds(&h));

        lock_a.unlock_and_destroy();
        a.pump(&mut h);
        assert_eq!(a.error(), None);
        assert!(!h.state.lock.locked);
        assert!(!h.state.lock.locker_alive());
    }

    /// A lock command naming this test binary, so the in-process client is
    /// "the locker". `--list` only in case anything ever ran it.
    fn this_binary_as_locker() -> String {
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        format!("{} --list", exe.display())
    }

    /// Item 2: a locker that dies leaves the session locked behind the
    /// fallback, a relaunch is scheduled, and the lock command's program takes
    /// over the ownerless session without it ever reading unlocked.
    #[test]
    fn a_dead_locker_stays_locked_and_a_replacement_takes_over() {
        let mut h = harness();
        h.state.config.idle.lock_command = Some(this_binary_as_locker());
        let output = h.state.outputs.get(h.a).expect("output").output.clone();
        let mut a = Client::connect(&mut h);
        let lock_a = a.lock(&mut h);
        a.lock_surface(&mut h, &lock_a);
        assert!(h.state.lock.surface_for(&output).is_some());

        h.state
            .display_handle
            .backend_handle()
            .kill_client(a.server.id(), DisconnectReason::ConnectionClosed);
        for _ in 0..4 {
            h.dispatch();
            assert!(h.state.lock.locked, "a dead locker never unlocks");
        }
        assert!(!h.state.lock.locker_alive());
        assert!(h.state.lock.surface_for(&output).is_none(), "the fallback shows");
        assert!(h.state.lock.relaunch.next.is_some(), "a relaunch is scheduled");
        assert!(h.state.lock.pending(Instant::now()));

        let mut c = Client::connect(&mut h);
        let lock_c = c.lock(&mut h);
        assert_eq!((c.seen.locked, c.seen.finished), (1, 0), "the replacement locks");
        assert!(h.state.lock.locked && c.holds(&h));
        assert!(!h.state.lock.pending(Instant::now()));
        assert!(
            h.state.lock.relaunch.next.is_none(),
            "no relaunch once taken over"
        );
        c.lock_surface(&mut h, &lock_c);
        assert!(h.state.lock.surface_for(&output).is_some());

        lock_c.unlock_and_destroy();
        c.pump(&mut h);
        assert!(!h.state.lock.locked);
        assert_eq!(h.state.lock.relaunch.attempts, 0, "unlock resets the backoff");
        drop(lock_a);
    }

    /// Takeover is refused to a client that is not the lock command's program,
    /// and to everyone when no lock command is set: `finished`, still locked,
    /// still ownerless.
    #[test]
    fn takeover_is_refused_to_anything_but_the_lock_command() {
        for cmd in [None, Some("true".to_owned())] {
            let mut h = harness();
            h.state.config.idle.lock_command = cmd.clone();
            let mut a = Client::connect(&mut h);
            let _lock_a = a.lock(&mut h);
            h.state
                .display_handle
                .backend_handle()
                .kill_client(a.server.id(), DisconnectReason::ConnectionClosed);
            for _ in 0..4 {
                h.dispatch();
            }
            assert!(h.state.lock.locked && !h.state.lock.locker_alive());

            let mut c = Client::connect(&mut h);
            let lock_c = c.lock(&mut h);
            assert_eq!((c.seen.locked, c.seen.finished), (0, 1), "{cmd:?}: refused");
            assert!(h.state.lock.locked, "{cmd:?}: still locked");
            assert!(
                !h.state.lock.locker_alive() && !c.holds(&h),
                "{cmd:?}: still ownerless"
            );
            c.lock_surface(&mut h, &lock_c);
            assert!(
                h.state.lock.surfaces.iter().all(|(_, s)| !s.alive()),
                "{cmd:?}: draws nothing"
            );
            lock_c.unlock_and_destroy();
            c.pump(&mut h);
            assert!(c.error().is_some());
            assert!(h.state.lock.locked, "{cmd:?}: a refused locker cannot unlock");
        }
    }

    #[test]
    fn the_locker_program_is_argv0_resolved_without_path() {
        let sh = std::fs::canonicalize("/usr/bin/sh").ok();
        assert_eq!(locker_program("sh -c x"), sh, "bare name is /usr/bin/<name>");
        assert_eq!(
            locker_program("/usr/bin/sh"),
            sh,
            "absolute path, symlinks resolved"
        );
        assert_eq!(locker_program(""), None);
        assert_eq!(locker_program("bin/sh"), None, "relative path");
        assert_eq!(locker_program("\"/usr/bin/sh\""), None, "quoted");
        assert_eq!(locker_program("FOO=1 sh"), None, "env assignment");
        assert_eq!(locker_program("$HOME/locker"), None, "shell expansion");
        assert_eq!(locker_program("no-such-locker-ec-test"), None, "does not resolve");
    }

    /// Destroying a held lock without unlocking is the protocol's
    /// `invalid_destroy`, and leaves the session locked like a crash does.
    #[test]
    fn destroy_while_locked_stays_locked() {
        let mut h = harness();
        let mut a = Client::connect(&mut h);
        let lock_a = a.lock(&mut h);
        lock_a.destroy();
        a.pump(&mut h);
        assert_eq!(a.error(), Some(u32::from(clock::Error::InvalidDestroy)));
        assert!(h.state.lock.locked && !h.state.lock.locker_alive());
        // No lock-command: nothing to relaunch.
        assert!(h.state.lock.relaunch.next.is_none());
    }

    /// The backoff: 1, 2, 4, 8, 8 s, then it stops, however long the session
    /// stays ownerless.
    #[test]
    fn relaunch_backs_off_and_stops() {
        let mut lock = LockState {
            locked: true,
            ..LockState::default()
        };
        let t0 = Instant::now();
        assert_eq!(lock.schedule_relaunch(t0), Some(Duration::from_secs(1)));
        assert!(!lock.poll_relaunch(t0), "not due yet");
        let mut now = t0;
        let mut waits = Vec::new();
        while let Some(at) = lock.relaunch.next {
            waits.push(at - now);
            now = at;
            assert!(lock.poll_relaunch(now));
            assert!(!lock.poll_relaunch(now), "one relaunch per due time");
        }
        let secs: Vec<u64> = waits.iter().map(Duration::as_secs).collect();
        assert_eq!(secs, [1, 2, 4, 8, 8]);
        assert_eq!(lock.relaunch.attempts, RELAUNCH_BACKOFF.len());
        assert!(!lock.poll_relaunch(now + Duration::from_secs(3600)));
        // A later death in the same locked session gets nothing more.
        assert_eq!(lock.schedule_relaunch(now), None);

        // Unlocked: a scheduled relaunch is dropped, not run.
        let mut lock = LockState::default();
        lock.schedule_relaunch(t0);
        assert!(!lock.poll_relaunch(t0 + Duration::from_secs(5)));
        assert!(lock.relaunch.next.is_none());
    }

    /// `get_lock_state` and the `lock` event carry the same three fields.
    #[test]
    fn lock_state_is_reported_and_announced() {
        let mut h = harness();
        let _ = crate::ipc::capture::take();
        assert_eq!(
            lock_payload(&h.state),
            json!({"locked": false, "pending": false, "locker_alive": false})
        );
        let mut a = Client::connect(&mut h);
        let lock_a = a.lock(&mut h);
        let events: Vec<Value> = crate::ipc::capture::take()
            .into_iter()
            .filter(|(k, _)| k == "lock")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(
            events.last(),
            Some(&json!({"locked": true, "pending": false, "locker_alive": true}))
        );
        lock_a.unlock_and_destroy();
        a.pump(&mut h);
        let last = crate::ipc::capture::take()
            .into_iter()
            .filter(|(k, _)| k == "lock")
            .map(|(_, v)| v)
            .next_back();
        assert_eq!(
            last,
            Some(json!({"locked": false, "pending": false, "locker_alive": false}))
        );
    }
}
