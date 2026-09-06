// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_data_control_v1` — clipboard managers (COMP-06 §4).
//!
//! Data control reads *every* selection, so the global is only visible to
//! clients named in `clipboard { data-control-allow ... }`. The filter is
//! fail-closed: a client whose identity cannot be determined does not see the
//! global at all, and any bind attempt is refused by wayland-server.
//!
//! Identity is the peer pid from `SO_PEERCRED` resolved through
//! `/proc/<pid>/comm` (falling back to the basename of `/proc/<pid>/exe`).
//! This is a stopgap: pids are reusable and `comm` is writable by the process
//! itself. See ADR 0022.
//!
//! TODO(COMP-05): replace with the app identity/provenance record once app
//! identity exists, and drop the /proc lookup entirely.

use smithay::{
    delegate_data_control,
    reexports::wayland_server::{Client, DisplayHandle},
    wayland::selection::wlr_data_control::{DataControlHandler, DataControlState},
};

use crate::state::HeliosState;

impl DataControlHandler for HeliosState {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

delegate_data_control!(HeliosState);

/// Best-effort process name for a Wayland client. `None` when it cannot be
/// determined — callers must treat that as a denial.
pub fn client_name(dh: &DisplayHandle, client: &Client) -> Option<String> {
    let pid = client.get_credentials(dh).ok()?.pid;
    if pid <= 0 {
        return None;
    }
    if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        let comm = comm.trim();
        if !comm.is_empty() {
            return Some(comm.to_string());
        }
    }
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(exe.file_name()?.to_string_lossy().into_owned())
}

/// Build the fail-closed visibility filter for the data-control global.
///
/// The allowlist is captured here, so it is *not* hot-reloadable — a config
/// reload that changes it needs a compositor restart (TODO, M6).
pub fn allow_filter(
    dh: DisplayHandle,
    allow: Vec<String>,
) -> impl for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static {
    move |client: &Client| {
        if allow.is_empty() {
            return false;
        }
        match client_name(&dh, client) {
            Some(name) if allow.iter().any(|a| a == &name) => true,
            Some(name) => {
                tracing::warn!(client = %name, "wlr_data_control denied (not in clipboard.data-control-allow)");
                false
            }
            None => {
                tracing::warn!("wlr_data_control denied (client identity unknown)");
                false
            }
        }
    }
}
