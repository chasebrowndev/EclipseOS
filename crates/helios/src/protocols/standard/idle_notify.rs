// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_idle_notify_v1` (COMP-03 §7). The clock itself lives in
//! [`crate::input::idle`]; this only wires the protocol state up.

use smithay::{
    delegate_idle_notify,
    wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState},
};

use crate::state::HeliosState;

impl IdleNotifierHandler for HeliosState {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier
    }
}

delegate_idle_notify!(HeliosState);
