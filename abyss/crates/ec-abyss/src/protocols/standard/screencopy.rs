// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_screencopy_v1` — screen capture for the xdg-desktop-portal
//! ScreenCast backend (COMP-06 §1, §3; COMP-16 M8).
//!
//! Smithay 0.7 ships no screencopy implementation, so this is hand-written
//! against the protocol XML.
//!
//! # Trust boundary
//!
//! Capture reads every pixel of an output, including other clients' windows.
//! There is no ambient authority here and no ungated path: a capture is
//! authorised only if the requesting client's executable is admitted by
//! `capture { allow ... }`, exactly as `wlr_data_control` is gated (ADR 0022,
//! matched by full executable path since ADR 0094).
//! `policyd` does not exist yet, so with no allowlist configured — the default
//! — **every** capture is denied. The gate is applied twice: as a global
//! visibility filter, so a denied client never sees the manager at all, and
//! again per request, because a client that somehow holds the global must
//! still not get pixels.
//!
//! No state is mutated and no buffer is allocated before the gate returns
//! [`Decision::Allow`]: a denied `capture_output` initialises the new frame
//! object (the protocol requires it) with no target and sends `failed` at
//! once, so no `buffer` event, no offscreen texture and no queue entry ever
//! exist for it.
//!
//! `overlay_cursor` is the client's opt-in to bake the pointer into the copy
//! (COMP-02 §8: excluded unless asked). It rides on [`Target`] from the
//! `capture_output`/`capture_output_region` request through to the
//! [`capture::Pending`] the `copy` request queues, and a trusted prompt
//! holding the seat still overrides it (`render::capture::copy_one`).

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::{
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Physical, Rectangle},
};

use std::path::Path;

use crate::{config::ExeAllowlist, render::capture, state::AbyssState};

/// Protocol version advertised. v3 adds `linux_dmabuf`/`buffer_done`; we
/// advertise the version but offer shm buffers only.
const VERSION: u32 = 3;

/// Why a capture request was allowed or refused. Fail-closed: every variant
/// except [`Decision::Allow`] denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// The session is locked. A locked session's contents are never capturable.
    DenyLocked,
    /// No allowlist configured. This is the default, and it denies.
    DenyNoAllowlist,
    /// `SO_PEERCRED` gave us no usable executable (unreadable, or deleted).
    DenyUnknownClient,
    /// Identified, but not allowed.
    DenyNotAllowed,
}

impl Decision {
    pub fn allowed(self) -> bool {
        matches!(self, Decision::Allow)
    }

    fn reason(self) -> &'static str {
        match self {
            Decision::Allow => "allowed",
            Decision::DenyLocked => "session is locked",
            Decision::DenyNoAllowlist => "no capture.allow entries configured",
            Decision::DenyUnknownClient => "client identity unknown",
            Decision::DenyNotAllowed => "client not in capture.allow",
        }
    }
}

/// The gate itself, as a pure function so it can be tested without a display.
///
/// Order matters: the lock check comes first so that no allowlist entry can
/// ever unlock capture of a locked session.
pub fn decide(allow: &ExeAllowlist, locked: bool, exe: Option<&Path>) -> Decision {
    if locked {
        return Decision::DenyLocked;
    }
    if allow.is_empty() {
        return Decision::DenyNoAllowlist;
    }
    match exe {
        None => Decision::DenyUnknownClient,
        Some(e) if allow.contains_exe(e) => Decision::Allow,
        Some(_) => Decision::DenyNotAllowed,
    }
}

/// The whole gate for one output: [`decide`] first (the lock, then the
/// allowlist), then a live screen-share grant for exactly this output
/// (ADR 0097). A grant never lifts the lock.
pub fn admit(state: &AbyssState, exe: Option<&Path>, output_id: u64) -> Decision {
    let d = decide(&state.capture_allow, state.lock.locked, exe);
    if d.allowed() || d == Decision::DenyLocked {
        return d;
    }
    match exe {
        Some(e) if crate::trusted_ui::screencast::granted(state, e, output_id) => Decision::Allow,
        _ => d,
    }
}

/// Global visibility: the allowlist, or the portal, which can bind but
/// captures nothing without a grant.
fn visible(allow: &ExeAllowlist, exe: Option<&Path>) -> bool {
    decide(allow, false, exe).allowed() || crate::trusted_ui::screencast::is_portal(exe)
}

/// Global data for the manager: enough to identify a client at bind time.
#[derive(Debug)]
pub struct ManagerData {
    dh: crate::protocols::standard::data_control::WeakDh,
    allow: ExeAllowlist,
}

/// What a frame object is allowed to copy. `None` means the request was
/// denied; the object exists only because the protocol demands the new_id be
/// initialised, and every request on it fails.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub output_id: u64,
    /// Output-local, physical.
    pub region: Rectangle<i32, Physical>,
    /// The client's `overlay_cursor` ask (COMP-02 §8: opt-in, never ambient).
    pub overlay_cursor: bool,
}

#[derive(Debug)]
pub struct FrameData {
    pub target: Option<Target>,
    /// A frame may be copied once (`already_used`).
    used: AtomicBool,
    with_damage: bool,
}

#[derive(Debug)]
pub struct ScreencopyState;

impl ScreencopyState {
    /// Register the global. The bind filter holds an [`ExeAllowlist`] handle, so
    /// a config reload retunes it without a restart (ADR 0022 amendment).
    pub fn new(
        dh: &DisplayHandle,
        weak_dh: crate::protocols::standard::data_control::WeakDh,
        allow: ExeAllowlist,
    ) -> Self {
        if allow.is_empty() {
            tracing::info!("screencopy: no capture.allow entries, all capture denied");
        } else {
            tracing::info!(allow = ?allow.names(), "screencopy: capture allowlist");
        }
        dh.create_global::<AbyssState, ZwlrScreencopyManagerV1, _>(
            VERSION,
            ManagerData { dh: weak_dh, allow },
        );
        Self
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ManagerData> for AbyssState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &ManagerData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global_data: &ManagerData) -> bool {
        // The lock state is not reachable from here, so this filter enforces
        // the allowlist only; `decide` re-checks the lock per request.
        let exe = global_data.dh.client_exe(&client);
        let shown = visible(&global_data.allow, exe.as_deref());
        if !shown {
            tracing::debug!(client = ?exe, "screencopy global hidden");
        }
        shown
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_manager_v1::Request;
        let (frame, output, region, with_damage, overlay_cursor) = match request {
            Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, output, None, false, overlay_cursor != 0),
            Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (
                frame,
                output,
                Some((x, y, width, height)),
                false,
                overlay_cursor != 0,
            ),
            Request::Destroy => return,
            _ => return,
        };

        // The gate runs before anything is computed, allocated or queued. A
        // screen-share grant is for one output, so the output is named first.
        let exe = crate::protocols::standard::data_control::client_exe(dh, client);
        let decision = match state.outputs.by_wl_output(&output).map(|e| e.id) {
            Some(id) => admit(state, exe.as_deref(), id),
            None => decide(&state.capture_allow, state.lock.locked, exe.as_deref()),
        };
        if !decision.allowed() {
            tracing::warn!(
                client = ?exe,
                reason = decision.reason(),
                "screencopy denied"
            );
            let frame = data_init.init(
                frame,
                FrameData {
                    target: None,
                    used: AtomicBool::new(true),
                    with_damage,
                },
            );
            frame.failed();
            return;
        }

        let Some(entry) = state.outputs.by_wl_output(&output) else {
            tracing::warn!("screencopy: unknown wl_output");
            let frame = data_init.init(
                frame,
                FrameData {
                    target: None,
                    used: AtomicBool::new(true),
                    with_damage,
                },
            );
            frame.failed();
            return;
        };
        let output_id = entry.id;
        let wl_out = entry.output.clone();
        let scale = wl_out.current_scale().fractional_scale();
        let Some(mode) = wl_out.current_mode() else {
            let frame = data_init.init(
                frame,
                FrameData {
                    target: None,
                    used: AtomicBool::new(true),
                    with_damage,
                },
            );
            frame.failed();
            return;
        };
        let full: Rectangle<i32, Physical> = Rectangle::from_size(mode.size);
        let rect = match region {
            None => full,
            Some((x, y, w, h)) => {
                let r: Rectangle<i32, smithay::utils::Logical> = Rectangle::new((x, y).into(), (w, h).into());
                let r = r.to_f64().to_physical(scale).to_i32_round();
                match r.intersection(full) {
                    Some(r) if r.size.w > 0 && r.size.h > 0 => r,
                    _ => {
                        let frame = data_init.init(
                            frame,
                            FrameData {
                                target: None,
                                used: AtomicBool::new(true),
                                with_damage,
                            },
                        );
                        frame.failed();
                        return;
                    }
                }
            }
        };

        tracing::info!(
            client = ?exe,
            output = %entry.identity,
            w = rect.size.w,
            h = rect.size.h,
            "screencopy allowed"
        );
        let frame = data_init.init(
            frame,
            FrameData {
                target: Some(Target {
                    output_id,
                    region: rect,
                    overlay_cursor,
                }),
                used: AtomicBool::new(false),
                with_damage,
            },
        );
        frame.buffer(
            capture::SHM_FORMAT,
            rect.size.w as u32,
            rect.size.h as u32,
            rect.size.w as u32 * 4,
        );
        if manager.version() >= 3 {
            // No dmabuf offered: the shm path is the only one implemented.
            frame.buffer_done();
        }
        // The indicator is persistent for as long as capture keeps happening.
        state.capture_seen = Some(std::time::Instant::now());
        state.capture_consumer = exe;
        super::capture_event::sync(state);
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_frame_v1::Request;
        let (buffer, with_damage) = match request {
            Request::Copy { buffer } => (buffer, false),
            Request::CopyWithDamage { buffer } => (buffer, true),
            Request::Destroy => return,
            _ => return,
        };

        if data.used.swap(true, Ordering::Relaxed) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "frame already copied",
            );
            return;
        }
        // A frame that was denied has no target, and never gains one.
        let Some(target) = data.target else {
            frame.failed();
            return;
        };
        // Re-check: a client may have held an authorised frame across a lock,
        // a reload, or the end of its screen-share grant.
        let exe = crate::protocols::standard::data_control::client_exe(dh, client);
        if !admit(state, exe.as_deref(), target.output_id).allowed() {
            tracing::warn!("screencopy copy denied at copy time");
            frame.failed();
            return;
        }
        if let Err(e) = validate(&buffer, target.region) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, e);
            return;
        }

        state.captures.push(capture::Pending {
            sink: capture::Sink::Wlr(frame.clone()),
            buffer,
            output_id: target.output_id,
            region: target.region,
            with_damage: with_damage || data.with_damage,
            overlay_cursor: target.overlay_cursor,
        });
        crate::backend::damage_all(state);
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        frame: &ZwlrScreencopyFrameV1,
        _data: &FrameData,
    ) {
        state
            .captures
            .retain(|p| p.sink != capture::Sink::Wlr(frame.clone()));
    }
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

/// `wl_shm` formats the compositor must advertise for capture buffers to be
/// creatable at all. `Argb8888`/`Xrgb8888` are implicit in the protocol.
pub const EXTRA_SHM_FORMATS: &[wl_shm::Format] = &[capture::SHM_FORMAT];

#[allow(dead_code)]
fn _assert_output_type(_: &WlOutput) {}

#[cfg(test)]
mod tests {
    use super::{decide, Decision, ExeAllowlist};
    use std::path::Path;

    /// Not installed anywhere, so the entry stays literal on every host.
    const GRIM: &str = "/usr/bin/ec-test-grim";

    fn allow() -> ExeAllowlist {
        ExeAllowlist::new(vec![GRIM.to_string()])
    }

    fn empty() -> ExeAllowlist {
        ExeAllowlist::default()
    }

    fn grim() -> &'static Path {
        Path::new(GRIM)
    }

    #[test]
    fn fail_closed_by_default() {
        // No configuration at all: nothing captures, whoever asks.
        assert_eq!(decide(&empty(), false, Some(grim())), Decision::DenyNoAllowlist);
        assert_eq!(decide(&empty(), false, None), Decision::DenyNoAllowlist);
    }

    #[test]
    fn allowlisted_client_is_allowed() {
        assert_eq!(decide(&allow(), false, Some(grim())), Decision::Allow);
        assert!(decide(&allow(), false, Some(grim())).allowed());
    }

    #[test]
    fn unlisted_and_unidentified_clients_are_denied() {
        assert_eq!(
            decide(&allow(), false, Some(Path::new("/usr/bin/evil"))),
            Decision::DenyNotAllowed
        );
        assert_eq!(decide(&allow(), false, None), Decision::DenyUnknownClient);
    }

    /// SEC-04: the basename is not the identity. `/tmp/grim` is a different
    /// executable from the allowlisted one, whether the entry is the bare
    /// name or the path.
    #[test]
    fn a_copy_under_an_allowlisted_name_is_denied() {
        for entry in ["ec-test-grim", GRIM] {
            let allow = ExeAllowlist::new(vec![entry.to_string()]);
            assert_eq!(
                decide(&allow, false, Some(Path::new("/tmp/ec-test-grim"))),
                Decision::DenyNotAllowed,
                "{entry}"
            );
        }
    }

    #[test]
    fn lock_beats_the_allowlist() {
        // The lock check is first, so no allowlist entry can capture a locked
        // session.
        assert_eq!(decide(&allow(), true, Some(grim())), Decision::DenyLocked);
        assert_eq!(decide(&empty(), true, Some(grim())), Decision::DenyLocked);
    }
}
