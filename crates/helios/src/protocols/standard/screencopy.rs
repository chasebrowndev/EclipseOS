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
//! authorised only if the requesting client's process name appears in
//! `capture { allow ... }`, exactly as `wlr_data_control` is gated (ADR 0022).
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

use crate::{config::Allowlist, render::capture, state::HeliosState};

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
    /// `SO_PEERCRED` gave us no usable process name.
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
pub fn decide(allow: &Allowlist, locked: bool, name: Option<&str>) -> Decision {
    if locked {
        return Decision::DenyLocked;
    }
    if allow.is_empty() {
        return Decision::DenyNoAllowlist;
    }
    match name {
        None => Decision::DenyUnknownClient,
        Some(n) if allow.contains(n) => Decision::Allow,
        Some(_) => Decision::DenyNotAllowed,
    }
}

/// Global data for the manager: enough to identify a client at bind time.
#[derive(Debug)]
pub struct ManagerData {
    dh: DisplayHandle,
    allow: Allowlist,
}

/// What a frame object is allowed to copy. `None` means the request was
/// denied; the object exists only because the protocol demands the new_id be
/// initialised, and every request on it fails.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub output_id: u64,
    /// Output-local, physical.
    pub region: Rectangle<i32, Physical>,
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
    /// Register the global. The bind filter holds an [`Allowlist`] handle, so
    /// a config reload retunes it without a restart (ADR 0022 amendment).
    pub fn new(dh: &DisplayHandle, allow: Allowlist) -> Self {
        if allow.is_empty() {
            tracing::info!("screencopy: no capture.allow entries, all capture denied");
        } else {
            tracing::info!(allow = ?allow.names(), "screencopy: capture allowlist");
        }
        dh.create_global::<HeliosState, ZwlrScreencopyManagerV1, _>(
            VERSION,
            ManagerData {
                dh: dh.clone(),
                allow,
            },
        );
        Self
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ManagerData> for HeliosState {
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
        let name = crate::protocols::standard::data_control::client_name(&global_data.dh, &client);
        let d = decide(&global_data.allow, false, name.as_deref());
        if !d.allowed() {
            tracing::debug!(
                client = name.as_deref().unwrap_or("<unknown>"),
                reason = d.reason(),
                "screencopy global hidden"
            );
        }
        d.allowed()
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for HeliosState {
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
        let (frame, output, region, with_damage) = match request {
            Request::CaptureOutput {
                frame,
                overlay_cursor: _,
                output,
            } => (frame, output, None, false),
            Request::CaptureOutputRegion {
                frame,
                overlay_cursor: _,
                output,
                x,
                y,
                width,
                height,
            } => (frame, output, Some((x, y, width, height)), false),
            Request::Destroy => return,
            _ => return,
        };

        // The gate runs before anything is computed, allocated or queued.
        let name = crate::protocols::standard::data_control::client_name(dh, client);
        let decision = decide(&state.capture_allow, state.lock.locked, name.as_deref());
        if !decision.allowed() {
            tracing::warn!(
                client = name.as_deref().unwrap_or("<unknown>"),
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
            client = name.as_deref().unwrap_or("<unknown>"),
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
        state.capture_consumer = name;
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for HeliosState {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
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
        // Re-check: a client may have held an authorised frame across a lock.
        if state.lock.locked {
            tracing::warn!("screencopy copy denied (session locked)");
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
    use super::{decide, Allowlist, Decision};

    fn allow() -> Allowlist {
        Allowlist::new(vec!["grim".to_string()])
    }

    fn empty() -> Allowlist {
        Allowlist::default()
    }

    #[test]
    fn fail_closed_by_default() {
        // No configuration at all: nothing captures, whoever asks.
        assert_eq!(decide(&empty(), false, Some("grim")), Decision::DenyNoAllowlist);
        assert_eq!(decide(&empty(), false, None), Decision::DenyNoAllowlist);
    }

    #[test]
    fn allowlisted_client_is_allowed() {
        assert_eq!(decide(&allow(), false, Some("grim")), Decision::Allow);
        assert!(decide(&allow(), false, Some("grim")).allowed());
    }

    #[test]
    fn unlisted_and_unidentified_clients_are_denied() {
        assert_eq!(decide(&allow(), false, Some("evil")), Decision::DenyNotAllowed);
        assert_eq!(decide(&allow(), false, None), Decision::DenyUnknownClient);
    }

    #[test]
    fn lock_beats_the_allowlist() {
        // The lock check is first, so no allowlist entry can capture a locked
        // session.
        assert_eq!(decide(&allow(), true, Some("grim")), Decision::DenyLocked);
        assert_eq!(decide(&empty(), true, Some("grim")), Decision::DenyLocked);
    }
}
