// SPDX-License-Identifier: AGPL-3.0-only
use std::os::unix::io::OwnedFd;

use smithay::{
    delegate_data_device,
    input::Seat,
    wayland::selection::{
        data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
        SelectionHandler,
    },
};

use crate::state::HeliosState;

impl SelectionHandler for HeliosState {
    type SelectionUserData = ();
}

impl DataDeviceHandler for HeliosState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for HeliosState {}
impl ServerDndGrabHandler for HeliosState {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

delegate_data_device!(HeliosState);
