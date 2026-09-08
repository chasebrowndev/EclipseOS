// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_data_control_v1` — clipboard managers (COMP-06 §4).
//!
//! Data control reads *every* selection, so the global is only visible to
//! clients named in `clipboard { data-control-allow ... }`. The filter is
//! fail-closed: a client whose identity cannot be determined does not see the
//! global at all, and any bind attempt is refused by wayland-server.
//!
//! Identity is the peer pid from `SO_PEERCRED` resolved through the basename
//! of `/proc/<pid>/exe` (falling back to `/proc/<pid>/comm`). This is a
//! stopgap: pids are reusable and a binary can be copied under an allowlisted
//! name. See ADR 0022.
//!
//! TODO(COMP-05): replace with the app identity/provenance record once app
//! identity exists, and drop the /proc lookup entirely.

use smithay::{
    delegate_data_control,
    reexports::wayland_server::{Client, DisplayHandle},
    wayland::selection::wlr_data_control::{DataControlHandler, DataControlState},
};

use crate::{config::Allowlist, state::AbyssState};

impl DataControlHandler for AbyssState {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

delegate_data_control!(AbyssState);

/// Best-effort process name for a Wayland client. `None` when it cannot be
/// determined — callers must treat that as a denial.
pub fn client_name(dh: &DisplayHandle, client: &Client) -> Option<String> {
    let pid = client.get_credentials(dh).ok()?.pid;
    if pid <= 0 {
        return None;
    }
    // `exe` first: it is maintained by the kernel, is not writable by the
    // process, and is not truncated. `comm` is capped at 15 bytes, so every
    // `xdg-desktop-portal-*` backend collapses to `xdg-desktop-por` and one
    // allowlist entry would admit all of them.
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        if let Some(name) = exe.file_name() {
            let name = name.to_string_lossy();
            if !name.is_empty() {
                return Some(name.into_owned());
            }
        }
    }
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let comm = comm.trim();
    if comm.is_empty() {
        return None;
    }
    Some(comm.to_string())
}

/// Build the fail-closed visibility filter for the data-control global.
///
/// The filter holds an [`Allowlist`] handle rather than a snapshot, so a
/// config reload takes effect on the next bind without a restart. Clients
/// that already hold the global keep it — a filter is only consulted when a
/// client asks what globals exist.
pub fn allow_filter(
    dh: DisplayHandle,
    allow: Allowlist,
) -> impl for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static {
    move |client: &Client| {
        if allow.is_empty() {
            return false;
        }
        match client_name(&dh, client) {
            Some(name) if allow.contains(&name) => true,
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
