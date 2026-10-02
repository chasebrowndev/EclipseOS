// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_output_power_management_v1` (COMP-03 §7).
//!
//! Smithay 0.7 has no module for this one, so the two dispatches are written
//! out here against the wlr protocol bindings it re-exports. The compositor
//! stays the authority: a client request goes through
//! [`crate::outputs::power::set_power`] like any other, and every instance is
//! told the resulting mode.

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::output_power_management::v1::server::{
            zwlr_output_power_manager_v1::{Request as ManagerRequest, ZwlrOutputPowerManagerV1},
            zwlr_output_power_v1::{Mode, Request, ZwlrOutputPowerV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        },
    },
};

use crate::state::AbyssState;

const VERSION: u32 = 1;

/// User data on a `zwlr_output_power_v1`: the output it controls.
pub struct OutputPowerData {
    /// `None` once the output is gone; such an object only ever `failed`.
    output: Option<Output>,
}

pub struct OutputPowerState {
    #[allow(dead_code)] // holds the global alive
    global: GlobalId,
    instances: Vec<ZwlrOutputPowerV1>,
}

impl OutputPowerState {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<AbyssState, ZwlrOutputPowerManagerV1, _>(VERSION, ());
        Self {
            global,
            instances: Vec::new(),
        }
    }
}

/// Tell every interested client the new mode of one output.
pub fn broadcast(state: &mut AbyssState, id: u64, on: bool) {
    let Some(output) = state.outputs.get(id).map(|e| e.output.clone()) else {
        return;
    };
    let mode = if on { Mode::On } else { Mode::Off };
    for instance in &state.output_power.instances {
        if instance
            .data::<OutputPowerData>()
            .is_some_and(|d| d.output.as_ref() == Some(&output))
        {
            instance.mode(mode);
        }
    }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrOutputPowerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(manager, ());
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrOutputPowerManagerV1,
        request: ManagerRequest,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ManagerRequest::GetOutputPower { id, output } => {
                let output = Output::from_resource(&output);
                let Some(output) = output else {
                    // The output is gone; the object is created dead, as the
                    // protocol's `failed` event requires.
                    let power = data_init.init(id, OutputPowerData { output: None });
                    power.failed();
                    return;
                };
                let on = state
                    .outputs
                    .by_output(&output)
                    .map(|e| e.powered)
                    .unwrap_or(true);
                let power = data_init.init(id, OutputPowerData { output: Some(output) });
                power.mode(if on { Mode::On } else { Mode::Off });
                state.output_power.instances.push(power);
            }
            ManagerRequest::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, OutputPowerData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        power: &ZwlrOutputPowerV1,
        request: Request,
        data: &OutputPowerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            Request::SetMode { mode } => {
                let on = match mode {
                    WEnum::Value(Mode::On) => true,
                    WEnum::Value(Mode::Off) => false,
                    other => {
                        power.post_error(
                            smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_v1::Error::InvalidMode,
                            format!("invalid mode {other:?}"),
                        );
                        return;
                    }
                };
                let Some(id) = data
                    .output
                    .as_ref()
                    .and_then(|o| state.outputs.by_output(o))
                    .map(|e| e.id)
                else {
                    power.failed();
                    return;
                };
                crate::outputs::power::set_power(state, id, on);
            }
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, power: &ZwlrOutputPowerV1, _data: &OutputPowerData) {
        state.output_power.instances.retain(|p| p != power);
    }
}
