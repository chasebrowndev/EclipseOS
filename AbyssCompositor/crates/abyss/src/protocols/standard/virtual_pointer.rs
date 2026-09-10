// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_virtual_pointer_v1` (COMP-04 §6).
//!
//! Smithay 0.7 has no module for this one, so the dispatches are written out
//! here against the wlr bindings it re-exports, following `output_power.rs`.
//!
//! The protocol is frame-batched by design: a client queues motion, buttons
//! and axis state and nothing is delivered until `frame`. So each device
//! carries a [`Pending`] batch in [`VirtualPointerState`], and `frame` replays
//! it through [`AbyssState::inject_pointer_absolute`] and friends — the same
//! entry points a real device uses, so idle activity, click-to-focus, cursor
//! clamping and lock suppression all still apply.

use smithay::{
    input::pointer::AxisFrame,
    output::Output,
    reexports::{
        wayland_protocols_wlr::virtual_pointer::v1::server::{
            zwlr_virtual_pointer_manager_v1::{Request as ManagerRequest, ZwlrVirtualPointerManagerV1},
            zwlr_virtual_pointer_v1::{Error, Request, ZwlrVirtualPointerV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            protocol::{
                wl_output::WlOutput,
                wl_pointer::{Axis as WlAxis, AxisSource as WlAxisSource, ButtonState},
            },
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        },
    },
    utils::{Logical, Point},
};

use crate::state::AbyssState;

const VERSION: u32 = 2;

/// One frame's worth of queued events.
#[derive(Default)]
struct Pending {
    /// Accumulated relative motion.
    relative: Option<Point<f64, Logical>>,
    /// Absolute position, already mapped into global compositor coordinates.
    absolute: Option<Point<f64, Logical>>,
    /// `(button code, pressed)` in request order.
    buttons: Vec<(u32, bool)>,
    axis: Option<AxisFrame>,
    /// Timestamp of the last request in the batch.
    time: u32,
}

/// User data on a `zwlr_virtual_pointer_v1`: the output absolute motion maps
/// onto, if the client named one.
pub struct VirtualPointerData {
    output: Option<WlOutput>,
}

pub struct VirtualPointerState {
    #[allow(dead_code)] // holds the global alive
    global: GlobalId,
    devices: Vec<(ZwlrVirtualPointerV1, Pending)>,
}

impl VirtualPointerState {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<AbyssState, ZwlrVirtualPointerManagerV1, _>(VERSION, ());
        Self {
            global,
            devices: Vec::new(),
        }
    }
}

fn axis_of(value: WEnum<WlAxis>) -> Option<smithay::backend::input::Axis> {
    match value {
        WEnum::Value(WlAxis::HorizontalScroll) => Some(smithay::backend::input::Axis::Horizontal),
        WEnum::Value(WlAxis::VerticalScroll) => Some(smithay::backend::input::Axis::Vertical),
        _ => None,
    }
}

fn source_of(value: WEnum<WlAxisSource>) -> Option<smithay::backend::input::AxisSource> {
    use smithay::backend::input::AxisSource as S;
    match value {
        WEnum::Value(WlAxisSource::Wheel) => Some(S::Wheel),
        WEnum::Value(WlAxisSource::Finger) => Some(S::Finger),
        WEnum::Value(WlAxisSource::Continuous) => Some(S::Continuous),
        WEnum::Value(WlAxisSource::WheelTilt) => Some(S::WheelTilt),
        _ => None,
    }
}

impl GlobalDispatch<ZwlrVirtualPointerManagerV1, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrVirtualPointerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(manager, ());
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrVirtualPointerManagerV1,
        request: ManagerRequest,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        // The requested seat is ignored: abyss runs one seat (COMP-04 §2), and
        // the protocol allows a null seat to mean exactly that.
        let (id, output) = match request {
            ManagerRequest::CreateVirtualPointer { id, .. } => (id, None),
            ManagerRequest::CreateVirtualPointerWithOutput { id, output, .. } => (id, output),
            ManagerRequest::Destroy => return,
            _ => unreachable!(),
        };
        let device = data_init.init(id, VirtualPointerData { output });
        state.virtual_pointer.devices.push((device, Pending::default()));
    }
}

impl Dispatch<ZwlrVirtualPointerV1, VirtualPointerData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        device: &ZwlrVirtualPointerV1,
        request: Request,
        data: &VirtualPointerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `motion_absolute` needs the output geometry, which lives on `state`,
        // so it is resolved before the pending batch is borrowed.
        let absolute = match &request {
            Request::MotionAbsolute {
                x,
                y,
                x_extent,
                y_extent,
                ..
            } if *x_extent != 0 && *y_extent != 0 => {
                map_absolute(state, data, (*x, *y), (*x_extent, *y_extent))
            }
            _ => None,
        };
        let Some(pending) = state
            .virtual_pointer
            .devices
            .iter_mut()
            .find(|(d, _)| d == device)
            .map(|(_, p)| p)
        else {
            return;
        };
        match request {
            Request::Motion { time, dx, dy } => {
                pending.time = time;
                let delta = Point::from((dx, dy));
                pending.relative = Some(pending.relative.unwrap_or_default() + delta);
            }
            Request::MotionAbsolute { time, .. } => {
                pending.time = time;
                if let Some(location) = absolute {
                    pending.absolute = Some(location);
                }
            }
            Request::Button { time, button, state } => {
                pending.time = time;
                let pressed = match state {
                    WEnum::Value(ButtonState::Pressed) => true,
                    WEnum::Value(ButtonState::Released) => false,
                    _ => return,
                };
                pending.buttons.push((button, pressed));
            }
            Request::Axis { time, axis, value } => {
                pending.time = time;
                let Some(axis) = axis_of(axis) else {
                    device.post_error(Error::InvalidAxis, "invalid axis");
                    return;
                };
                let frame = pending.axis.take().unwrap_or_else(|| AxisFrame::new(time));
                pending.axis = Some(frame.value(axis, value));
            }
            Request::AxisDiscrete {
                time,
                axis,
                value,
                discrete,
            } => {
                pending.time = time;
                let Some(axis) = axis_of(axis) else {
                    device.post_error(Error::InvalidAxis, "invalid axis");
                    return;
                };
                let frame = pending.axis.take().unwrap_or_else(|| AxisFrame::new(time));
                // wl_pointer carries steps as v120; smithay divides by 120
                // again for the older `axis_discrete` event.
                pending.axis = Some(frame.value(axis, value).v120(axis, discrete * 120));
            }
            Request::AxisStop { time, axis } => {
                pending.time = time;
                let Some(axis) = axis_of(axis) else {
                    device.post_error(Error::InvalidAxis, "invalid axis");
                    return;
                };
                let frame = pending.axis.take().unwrap_or_else(|| AxisFrame::new(time));
                pending.axis = Some(frame.stop(axis));
            }
            Request::AxisSource { axis_source } => {
                let Some(source) = source_of(axis_source) else {
                    device.post_error(Error::InvalidAxisSource, "invalid axis source");
                    return;
                };
                let frame = pending
                    .axis
                    .take()
                    .unwrap_or_else(|| AxisFrame::new(pending.time));
                pending.axis = Some(frame.source(source));
            }
            Request::Frame => {
                let time = pending.time;
                let relative = pending.relative.take();
                let absolute = pending.absolute.take();
                let buttons = std::mem::take(&mut pending.buttons);
                let axis = pending.axis.take();
                if let Some(delta) = relative {
                    state.inject_pointer_relative(delta, time);
                }
                if let Some(location) = absolute {
                    state.inject_pointer_absolute(location, time);
                }
                for (button, pressed) in buttons {
                    state.inject_pointer_button(button, pressed, time);
                }
                if let Some(axis) = axis {
                    state.inject_pointer_axis(axis);
                }
            }
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        device: &ZwlrVirtualPointerV1,
        _data: &VirtualPointerData,
    ) {
        state.virtual_pointer.devices.retain(|(d, _)| d != device);
    }
}

/// Map `motion_absolute`'s device coordinates onto global compositor space.
///
/// The extents are the device's own coordinate range, so the position is a
/// fraction of the target output's logical geometry.
fn map_absolute(
    state: &AbyssState,
    data: &VirtualPointerData,
    pos: (u32, u32),
    extent: (u32, u32),
) -> Option<Point<f64, Logical>> {
    let output = data
        .output
        .as_ref()
        .and_then(Output::from_resource)
        .or_else(|| state.space.output_under(state.pointer_location).next().cloned())
        .or_else(|| state.space.outputs().next().cloned())?;
    let geo = state.space.output_geometry(&output)?;
    let x = geo.loc.x as f64 + pos.0 as f64 / extent.0 as f64 * geo.size.w as f64;
    let y = geo.loc.y as f64 + pos.1 as f64 / extent.1 as f64 * geo.size.h as f64;
    Some(Point::from((x, y)))
}
