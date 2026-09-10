// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_gamma_control_manager_v1` (COMP-06 §1) — the colour ramp a night-light
//! daemon loads onto an output.
//!
//! Smithay 0.7 has no module for this one, so the dispatches are written out
//! here against the wlr bindings it re-exports, following `output_power.rs`.
//!
//! A ramp changes what the human sees on a whole output, so the protocol's own
//! rule is the gate that matters: one client at a time per output. A second
//! client asking for a control on an output that already has one is told
//! `failed` and gets nothing, rather than silently fighting the first. Dropping
//! the control restores the identity ramp, so a crashed daemon cannot leave the
//! screen tinted.
//!
//! Backends without a programmable CRTC ramp (winit) answer `failed`.

use std::{io::Read, os::unix::io::OwnedFd};

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::gamma_control::v1::server::{
            zwlr_gamma_control_manager_v1::{Request as ManagerRequest, ZwlrGammaControlManagerV1},
            zwlr_gamma_control_v1::{Request, ZwlrGammaControlV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
};

use crate::state::AbyssState;

const VERSION: u32 = 1;

/// User data on a `zwlr_gamma_control_v1`: the output it drives.
pub struct GammaControlData {
    /// `None` once the output is gone; such an object only ever `failed`.
    output: Option<Output>,
}

#[derive(Default)]
pub struct GammaControlState {
    #[allow(dead_code)] // holds the global alive
    global: Option<GlobalId>,
    /// Live controls, at most one per output.
    instances: Vec<ZwlrGammaControlV1>,
}

impl GammaControlState {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<AbyssState, ZwlrGammaControlManagerV1, _>(VERSION, ());
        Self {
            global: Some(global),
            instances: Vec::new(),
        }
    }
}

/// Is some other control already driving this output?
fn taken(state: &AbyssState, output: &Output) -> bool {
    state.gamma_control.instances.iter().any(|c| {
        c.data::<GammaControlData>()
            .is_some_and(|d| d.output.as_ref() == Some(output))
    })
}

fn id_of(state: &AbyssState, output: &Output) -> Option<u64> {
    state.outputs.by_output(output).map(|e| e.id)
}

/// The ramp that means "unchanged": channel `i` maps to `i` scaled to 16 bits.
fn identity_ramp(size: u32) -> Vec<u16> {
    (0..size)
        .map(|i| ((i as u64 * u16::MAX as u64) / (size.max(2) - 1) as u64) as u16)
        .collect()
}

impl GlobalDispatch<ZwlrGammaControlManagerV1, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrGammaControlManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(manager, ());
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrGammaControlManagerV1,
        request: ManagerRequest,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ManagerRequest::GetGammaControl { id, output } => {
                let Some(output) = Output::from_resource(&output) else {
                    // Output already gone: create the object dead.
                    let control = data_init.init(id, GammaControlData { output: None });
                    control.failed();
                    return;
                };
                let size = id_of(state, &output)
                    .and_then(|id| crate::backend::gamma_size(state, id))
                    .filter(|_| !taken(state, &output));
                let control = data_init.init(id, GammaControlData { output: Some(output) });
                match size {
                    Some(size) => {
                        control.gamma_size(size);
                        state.gamma_control.instances.push(control);
                    }
                    // No ramp on this backend, or another client has it.
                    None => control.failed(),
                }
            }
            ManagerRequest::Destroy => {}
            _ => unreachable!(),
        }
    }
}

/// Read `size * 3` little-endian `u16`s from the client's fd, or `None` if the
/// fd is short, long, or unreadable. Bounded by `size`, so a client cannot make
/// the compositor read an unbounded amount.
fn read_ramps(fd: OwnedFd, size: u32) -> Option<(Vec<u16>, Vec<u16>, Vec<u16>)> {
    let bytes = size as usize * 3 * 2;
    let mut buf = vec![0u8; bytes + 1];
    let mut file = std::fs::File::from(fd);
    let mut read = 0;
    while read < buf.len() {
        match file.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    if read != bytes {
        return None;
    }
    let mut channels = buf[..bytes]
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect::<Vec<_>>();
    let b = channels.split_off(size as usize * 2);
    let g = channels.split_off(size as usize);
    Some((channels, g, b))
}

impl Dispatch<ZwlrGammaControlV1, GammaControlData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &ZwlrGammaControlV1,
        request: Request,
        data: &GammaControlData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            Request::SetGamma { fd } => {
                let Some(id) = data.output.as_ref().and_then(|o| id_of(state, o)) else {
                    control.failed();
                    return;
                };
                let Some(size) = crate::backend::gamma_size(state, id) else {
                    control.failed();
                    return;
                };
                let Some((r, g, b)) = read_ramps(fd, size) else {
                    control.post_error(
                        smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::Error::InvalidGamma,
                        "gamma ramp fd did not hold exactly one ramp",
                    );
                    return;
                };
                if !crate::backend::set_gamma(state, id, &r, &g, &b) {
                    control.failed();
                }
            }
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, control: &ZwlrGammaControlV1, data: &GammaControlData) {
        let held = state.gamma_control.instances.iter().any(|c| c == control);
        state.gamma_control.instances.retain(|c| c != control);
        if !held {
            return;
        }
        // Restore the screen: a daemon that died must not leave the output tinted.
        if let Some(id) = data.output.as_ref().and_then(|o| id_of(state, o)) {
            if let Some(size) = crate::backend::gamma_size(state, id) {
                let ramp = identity_ramp(size);
                crate::backend::set_gamma(state, id, &ramp, &ramp, &ramp);
            }
        }
    }
}
