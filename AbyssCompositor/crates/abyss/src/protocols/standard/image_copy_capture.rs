// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_image_copy_capture_v1` + `ext_image_capture_source_v1` — the staging
//! screen-capture protocol (COMP-02 §8, COMP-06 §3; COMP-16 M8).
//!
//! Smithay 0.7 ships no support for either protocol, so both are hand-written
//! against the XML, exactly as `zwlr_screencopy_v1` is.
//!
//! # Trust boundary
//!
//! This protocol reads the same pixels `zwlr_screencopy_v1` does, so it is
//! gated by the *same* function — [`screencopy::decide`] — and not by a copy
//! of it (ADR 0030). The gate is applied three times, in the same places:
//!
//! * as `GlobalDispatch::can_view` on both manager globals, so a client that
//!   is not allowlisted never sees them;
//! * again on `create_session` and on every `capture`, because a global may be
//!   bound before an allowlist reload;
//! * a third time in [`capture::service`], because a session may have
//!   outlived a lock.
//!
//! No state is mutated before the decision is `Allow`: a denied
//! `create_session` initialises the session object with no handle (the
//! protocol requires the new_id be initialised), sends `stopped` at once, and
//! never enters the session table.
//!
//! Redaction is not re-implemented either: an authorised capture is queued as
//! a [`capture::Pending`] on the one shared queue and serviced by
//! [`capture::service`], which builds its pass list with
//! [`capture::capture_elements`] — sensitive surfaces excluded, black quad in
//! their place.

use std::collections::HashMap;

use smithay::{
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::ext::{
            image_capture_source::v1::server::{
                ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
                ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
                ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
                ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{
            protocol::wl_buffer::WlBuffer, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
            Resource,
        },
    },
    utils::{Physical, Rectangle},
};

use crate::{
    config::Allowlist,
    protocols::standard::screencopy::{self, decide},
    render::capture,
    state::AbyssState,
};

/// Both interfaces are frozen at version 1.
const VERSION: u32 = 1;

/// Minimum spacing between two serviced frames of one session.
///
/// The protocol lets the compositor hold a `capture` for as long as it likes.
/// Servicing immediately is what makes continuous capture work at all, but a
/// client that re-arms the instant it sees `ready` would then composite as
/// fast as the CPU allows; one 60 Hz-shaped delay per frame bounds that
/// without ever making a frame conditional on damage that may not come.
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// Global data for both managers: enough to identify a client at bind time.
#[derive(Debug)]
pub struct ManagerData {
    dh: crate::protocols::standard::data_control::WeakDh,
    allow: Allowlist,
}

impl ManagerData {
    fn can_view(&self, client: &Client, what: &'static str) -> bool {
        // The lock state is not reachable here; `decide` re-checks it per
        // request and at service time.
        let name = self.dh.client_name(client);
        let d = decide(&self.allow, false, name.as_deref());
        if !d.allowed() {
            tracing::debug!(
                client = name.as_deref().unwrap_or("<unknown>"),
                global = what,
                "image-copy-capture global hidden"
            );
        }
        d.allowed()
    }
}

/// What a source object points at. `None` is an unusable source: the object
/// exists only because the protocol demands the new_id be initialised.
#[derive(Debug, Clone, Copy)]
pub struct SourceData {
    output_id: Option<u64>,
}

/// Handle into [`ImageCopyCaptureState::sessions`]. `None` means the session
/// was denied and is permanently stopped.
#[derive(Debug, Clone, Copy)]
pub struct SessionData {
    id: Option<u64>,
}

/// Handle into [`ImageCopyCaptureState::frames`].
#[derive(Debug, Clone, Copy)]
pub struct FrameData {
    id: Option<u64>,
}

/// Live session state. Kept here rather than in the object's user data so the
/// mutable half is owned by `AbyssState` and reachable only from the single
/// compositor thread — no interior mutability, no locks.
#[derive(Debug)]
struct Session {
    output_id: u64,
    /// Buffer size advertised to the client, in physical pixels.
    size: (i32, i32),
    /// The one live frame, if any (`duplicate_frame` is a protocol error).
    frame: Option<u64>,
    /// Has any frame of this session been serviced yet? The first frame always
    /// carries full damage.
    delivered: bool,
}

#[derive(Debug)]
struct Frame {
    session: u64,
    buffer: Option<WlBuffer>,
    captured: bool,
}

/// Holds the two globals alive and owns the session/frame tables.
#[derive(Debug)]
pub struct ImageCopyCaptureState {
    next: u64,
    sessions: HashMap<u64, Session>,
    frames: HashMap<u64, Frame>,
}

impl ImageCopyCaptureState {
    /// Register both globals. The bind filters hold an [`Allowlist`] handle,
    /// so a config reload retunes them without a restart (ADR 0022 amendment).
    pub fn new(
        dh: &DisplayHandle,
        weak_dh: crate::protocols::standard::data_control::WeakDh,
        allow: Allowlist,
    ) -> Self {
        dh.create_global::<AbyssState, ExtOutputImageCaptureSourceManagerV1, _>(
            VERSION,
            ManagerData {
                dh: weak_dh.clone(),
                allow: allow.clone(),
            },
        );
        dh.create_global::<AbyssState, ExtImageCopyCaptureManagerV1, _>(
            VERSION,
            ManagerData { dh: weak_dh, allow },
        );
        Self {
            next: 1,
            sessions: HashMap::new(),
            frames: HashMap::new(),
        }
    }

    fn alloc(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }
}

// --- ext_output_image_capture_source_manager_v1 ------------------------------

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ManagerData> for AbyssState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _global_data: &ManagerData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ManagerData) -> bool {
        global_data.can_view(&client, "ext_output_image_capture_source_manager_v1")
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_output_image_capture_source_manager_v1::Request;
        let Request::CreateSource { source, output } = request else {
            return;
        };
        // A source is a capability handle: gate it like everything else.
        let name = crate::protocols::standard::data_control::client_name(dh, client);
        let decision = decide(&state.capture_allow, state.lock.locked, name.as_deref());
        let output_id = if decision.allowed() {
            state.outputs.by_wl_output(&output).map(|e| e.id)
        } else {
            tracing::warn!(
                client = name.as_deref().unwrap_or("<unknown>"),
                "image capture source denied"
            );
            None
        };
        data_init.init(source, SourceData { output_id });
    }
}

impl Dispatch<ExtImageCaptureSourceV1, SourceData> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtImageCaptureSourceV1,
        _request: ext_image_capture_source_v1::Request,
        _data: &SourceData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `destroy` is the only request, and it is a destructor.
    }
}

// --- ext_image_copy_capture_manager_v1 ---------------------------------------

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ManagerData> for AbyssState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _global_data: &ManagerData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ManagerData) -> bool {
        global_data.can_view(&client, "ext_image_copy_capture_manager_v1")
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        _manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_manager_v1::Request;
        match request {
            Request::CreateSession {
                session,
                source,
                options: _,
            } => {
                // The gate runs before any table entry or event exists.
                let name = crate::protocols::standard::data_control::client_name(dh, client);
                let decision = decide(&state.capture_allow, state.lock.locked, name.as_deref());
                let source_data = source.data::<SourceData>().copied();
                let target = if decision.allowed() {
                    source_data.and_then(|s| s.output_id)
                } else {
                    None
                };
                let Some(output_id) = target else {
                    tracing::warn!(
                        client = name.as_deref().unwrap_or("<unknown>"),
                        "image copy capture session denied"
                    );
                    let session = data_init.init(session, SessionData { id: None });
                    session.stopped();
                    return;
                };
                let Some(size) = output_size(state, output_id) else {
                    let session = data_init.init(session, SessionData { id: None });
                    session.stopped();
                    return;
                };
                let id = state.image_copy.alloc();
                state.image_copy.sessions.insert(
                    id,
                    Session {
                        output_id,
                        size,
                        frame: None,
                        delivered: false,
                    },
                );
                let session = data_init.init(session, SessionData { id: Some(id) });
                session.buffer_size(size.0.max(0) as u32, size.1.max(0) as u32);
                session.shm_format(capture::SHM_FORMAT);
                // No dmabuf constraints: the shm path is the only one
                // implemented, exactly as for `zwlr_screencopy_v1`.
                session.done();
                tracing::info!(
                    client = name.as_deref().unwrap_or("<unknown>"),
                    w = size.0,
                    h = size.1,
                    "image copy capture session allowed"
                );
                state.capture_seen = Some(std::time::Instant::now());
                state.capture_consumer = name;
            }
            Request::CreatePointerCursorSession {
                session,
                source: _,
                pointer: _,
            } => {
                // Cursor capture is not implemented; hand back a session that
                // is stopped from the start rather than a silent no-op.
                let session = data_init.init(session, CursorSessionData);
                session.leave();
            }
            _ => {}
        }
    }
}

/// A cursor session that never produces anything.
#[derive(Debug)]
pub struct CursorSessionData;

impl Dispatch<ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1, CursorSessionData>
    for AbyssState
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        _data: &CursorSessionData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_cursor_session_v1::Request;
        if let Request::GetCaptureSession { session } = request {
            let session = data_init.init(session, SessionData { id: None });
            session.stopped();
        }
    }
}

use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_cursor_session_v1;

// --- ext_image_copy_capture_session_v1 ---------------------------------------

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        data: &SessionData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_session_v1::Request;
        let Request::CreateFrame { frame } = request else {
            return;
        };
        // A denied session never gains a handle, so its frames always fail.
        let Some(sid) = data.id else {
            let frame = data_init.init(frame, FrameData { id: None });
            frame.failed(FailureReason::Stopped);
            return;
        };
        let Some(session) = state.image_copy.sessions.get_mut(&sid) else {
            let frame = data_init.init(frame, FrameData { id: None });
            frame.failed(FailureReason::Stopped);
            return;
        };
        if session.frame.is_some() {
            resource.post_error(
                ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                "a frame already exists for this session",
            );
            return;
        }
        let fid = state.image_copy.alloc();
        state.image_copy.frames.insert(
            fid,
            Frame {
                session: sid,
                buffer: None,
                captured: false,
            },
        );
        if let Some(session) = state.image_copy.sessions.get_mut(&sid) {
            session.frame = Some(fid);
        }
        data_init.init(frame, FrameData { id: Some(fid) });
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        _resource: &ExtImageCopyCaptureSessionV1,
        data: &SessionData,
    ) {
        if let Some(id) = data.id {
            state.image_copy.sessions.remove(&id);
        }
    }
}

// --- ext_image_copy_capture_frame_v1 -----------------------------------------

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Request;
        let Some(fid) = data.id else {
            // Denied frames answer everything with `failed`.
            if !matches!(request, Request::Destroy) {
                resource.failed(FailureReason::Stopped);
            }
            return;
        };
        match request {
            Request::AttachBuffer { buffer } => {
                let Some(frame) = state.image_copy.frames.get_mut(&fid) else {
                    return;
                };
                if frame.captured {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "attach_buffer after capture",
                    );
                    return;
                }
                frame.buffer = Some(buffer);
            }
            Request::DamageBuffer { x, y, width, height } => {
                let Some(frame) = state.image_copy.frames.get(&fid) else {
                    return;
                };
                if frame.captured {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "damage_buffer after capture",
                    );
                    return;
                }
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage,
                        "damage rectangle out of range",
                    );
                }
                // Damage is advisory: the whole frame is copied regardless.
            }
            Request::Capture => capture_request(state, resource, fid),
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ExtImageCopyCaptureFrameV1,
        data: &FrameData,
    ) {
        state
            .captures
            .retain(|p| p.sink != capture::Sink::Ext(resource.clone()));
        let Some(fid) = data.id else { return };
        if let Some(frame) = state.image_copy.frames.remove(&fid) {
            if let Some(session) = state.image_copy.sessions.get_mut(&frame.session) {
                if session.frame == Some(fid) {
                    session.frame = None;
                }
            }
        }
    }
}

/// The `capture` request: gate, validate, queue, re-arm.
fn capture_request(state: &mut AbyssState, resource: &ExtImageCopyCaptureFrameV1, fid: u64) {
    let Some(frame) = state.image_copy.frames.get_mut(&fid) else {
        resource.failed(FailureReason::Stopped);
        return;
    };
    if frame.captured {
        resource.post_error(
            ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
            "capture sent twice",
        );
        return;
    }
    let Some(buffer) = frame.buffer.clone() else {
        resource.post_error(
            ext_image_copy_capture_frame_v1::Error::NoBuffer,
            "capture without attach_buffer",
        );
        return;
    };
    frame.captured = true;
    let sid = frame.session;
    let Some(session) = state.image_copy.sessions.get(&sid) else {
        resource.failed(FailureReason::Stopped);
        return;
    };
    let (output_id, size, delivered) = (session.output_id, session.size, session.delivered);

    // Re-check the gate: a session may have outlived a lock or a reload.
    let name = state.capture_consumer.clone();
    if decide(&state.capture_allow, state.lock.locked, name.as_deref()) != screencopy::Decision::Allow {
        tracing::warn!("image copy capture denied at capture time");
        resource.failed(FailureReason::Stopped);
        return;
    }
    // The output may have changed mode since the constraints were sent.
    if output_size(state, output_id) != Some(size) {
        resource.failed(FailureReason::BufferConstraints);
        return;
    }
    let region: Rectangle<i32, Physical> = Rectangle::from_size((size.0, size.1).into());
    if let Err(e) = validate(&buffer, region) {
        tracing::warn!(error = e, "image copy capture buffer rejected");
        resource.failed(FailureReason::BufferConstraints);
        return;
    }

    state.captures.push(capture::Pending {
        sink: capture::Sink::Ext(resource.clone()),
        buffer,
        output_id,
        region,
        with_damage: !delivered,
    });
    if let Some(session) = state.image_copy.sessions.get_mut(&sid) {
        session.delivered = true;
    }
    // Service on a short timer rather than immediately: continuous capture must
    // never depend on damage that may not come (a nested abyss whose host
    // window is occluded gets no frame callbacks), but it must not spin either.
    let _ = state.loop_handle.insert_source(
        Timer::from_duration(FRAME_INTERVAL),
        |_, _, state: &mut AbyssState| {
            crate::backend::damage_all(state);
            TimeoutAction::Drop
        },
    );
}

fn output_size(state: &AbyssState, output_id: u64) -> Option<(i32, i32)> {
    let entry = state.outputs.get(output_id)?;
    let mode = entry.output.current_mode()?;
    Some((mode.size.w, mode.size.h))
}

fn validate(buffer: &WlBuffer, region: Rectangle<i32, Physical>) -> Result<(), &'static str> {
    let data = match smithay::wayland::shm::with_buffer_contents(buffer, |_, _, d| d) {
        Ok(d) => d,
        Err(_) => return Err("not an shm buffer"),
    };
    if data.format != capture::SHM_FORMAT {
        return Err("wrong format");
    }
    if data.width != region.size.w || data.height != region.size.h {
        return Err("wrong dimensions");
    }
    if data.stride < region.size.w.saturating_mul(4) {
        return Err("stride too small");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        config::Allowlist,
        protocols::standard::screencopy::{decide, Decision},
    };

    /// The gate this protocol uses is the same function `zwlr_screencopy_v1`
    /// uses, so the ordering guarantee must hold for it too: a locked session
    /// denies whatever the allowlist says.
    #[test]
    fn lock_beats_the_allowlist() {
        let allow = Allowlist::new(vec!["xdg-desktop-portal-wlr".to_string()]);
        assert_eq!(
            decide(&allow, true, Some("xdg-desktop-portal-wlr")),
            Decision::DenyLocked
        );
        assert_eq!(
            decide(&Allowlist::default(), true, Some("xdg-desktop-portal-wlr")),
            Decision::DenyLocked
        );
        // And unlocked, the same client is the one that is allowed.
        assert_eq!(
            decide(&allow, false, Some("xdg-desktop-portal-wlr")),
            Decision::Allow
        );
    }

    #[test]
    fn sessions_are_denied_by_default() {
        assert_eq!(
            decide(&Allowlist::default(), false, Some("xdg-desktop-portal-wlr")),
            Decision::DenyNoAllowlist
        );
    }
}
