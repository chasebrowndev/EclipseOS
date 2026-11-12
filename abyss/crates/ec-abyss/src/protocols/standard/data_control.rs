// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_data_control_v1` — clipboard managers (COMP-06 §4).
//!
//! Data control reads *every* selection, so the global is only visible to
//! clients named in `clipboard { data-control-allow ... }`. The filter is
//! fail-closed: a client whose identity cannot be determined does not see the
//! global at all, and any bind attempt is refused by wayland-server.
//!
//! Identity is the peer pid from `SO_PEERCRED` resolved to the client's full
//! executable path through `/proc/<pid>/exe` ([`client_exe`]), matched
//! against `ExeAllowlist` entries (ADR 0094, amending 0022). This is still
//! containment, not authentication: pids are reusable and anything that can
//! write an allowlisted path can run as it.
//!
//! TODO(COMP-05): replace with the app identity/provenance record once app
//! identity exists, and drop the /proc lookup entirely.

use smithay::{
    delegate_data_control,
    reexports::wayland_server::{Client, DisplayHandle},
    wayland::selection::wlr_data_control::{DataControlHandler, DataControlState},
};

use std::path::PathBuf;

use crate::{config::ExeAllowlist, state::AbyssState};

impl DataControlHandler for AbyssState {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

delegate_data_control!(AbyssState);

/// The client's executable, fully resolved, for admission (ADR 0094). `None`
/// — which every caller reads as a denial — when the pid is unusable, when
/// `/proc/<pid>/exe` cannot be read, or when the binary it ran has since been
/// deleted or replaced. There is deliberately no `comm` fallback: `comm` is
/// whatever the process chose to call itself.
///
/// One `readlink`, the same cost the name lookup it replaces had: run at
/// global-visibility time and once per capture request, never per input event.
pub fn client_exe(dh: &DisplayHandle, client: &Client) -> Option<PathBuf> {
    exe_of_pid(client.get_credentials(dh).ok()?.pid)
}

/// [`client_exe`] for a bare pid.
pub fn exe_of_pid(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    admissible(std::fs::read_link(format!("/proc/{pid}/exe")).ok()?)
}

/// The pure half of [`exe_of_pid`]: a `/proc/<pid>/exe` link target is
/// admissible only if it is absolute and does not name a deleted file. The
/// kernel appends `" (deleted)"` once the inode is unlinked, so a binary
/// replaced on disk after it started no longer matches the path it came from.
pub fn admissible(link: PathBuf) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if !link.is_absolute() || link.as_os_str().as_bytes().ends_with(b" (deleted)") {
        return None;
    }
    Some(link)
}

/// Whether `client` is admitted by `allow`, logging a refusal at debug (a
/// visibility filter runs for every client that lists globals, so a warn
/// here would be one line per connection). Fail-closed throughout.
pub fn admit(
    dh: &WeakDh,
    allow: &ExeAllowlist,
    client: &Client,
    global: &'static str,
    key: &'static str,
) -> bool {
    if allow.is_empty() {
        return false;
    }
    match dh.client_exe(client) {
        Some(exe) if allow.contains_exe(&exe) => true,
        Some(exe) => {
            tracing::debug!(client = %exe.display(), global, key, "global hidden (executable not allowlisted)");
            false
        }
        None => {
            tracing::debug!(
                global,
                key,
                "global hidden (client executable unknown or deleted)"
            );
            false
        }
    }
}

/// Best-effort process *name* for a Wayland client, for logs and for the
/// lists that only ever tighten (`capture.hide-layer`) or that admit DE
/// clients to a cosmetic protocol (`eclipse_glass_v1`). Never an admission
/// decision for a security global: those use [`client_exe`]. `None` when it
/// cannot be determined.
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

/// A non-owning [`DisplayHandle`], for data the `Display` itself owns.
///
/// Global user data and bind filters are stored *inside* the display, so a
/// `DisplayHandle` parked in one is a strong reference cycle: the backend
/// never reaches refcount zero and every fd it owns (its epoll, eventfds,
/// timerfd, the seat's keymap memfd) leaks for the life of the process. That
/// is fatal under wlcs, which builds one compositor per test in one process.
/// Bind-time code holds this instead and upgrades through the `Arc` that
/// [`AbyssState`] owns.
#[derive(Debug, Clone)]
pub struct WeakDh(std::sync::Weak<DisplayHandle>);

impl WeakDh {
    pub fn new(dh: &std::sync::Arc<DisplayHandle>) -> Self {
        Self(std::sync::Arc::downgrade(dh))
    }

    /// [`client_name`] through the weak handle. Fail-closed: once the display
    /// is gone the identity is unknown, which callers must read as a denial.
    pub fn client_name(&self, client: &Client) -> Option<String> {
        let dh = self.0.upgrade()?;
        client_name(&dh, client)
    }

    /// [`client_exe`] through the weak handle; `None` once the display is gone.
    pub fn client_exe(&self, client: &Client) -> Option<PathBuf> {
        let dh = self.0.upgrade()?;
        client_exe(&dh, client)
    }
}

/// Build the fail-closed visibility filter for a `Client -> bool` global
/// (data control, input method).
///
/// The filter holds an [`ExeAllowlist`] handle rather than a snapshot, so a
/// config reload takes effect on the next bind without a restart. Clients
/// that already hold the global keep it — a filter is only consulted when a
/// client asks what globals exist.
pub fn allow_filter(
    dh: WeakDh,
    allow: ExeAllowlist,
    global: &'static str,
    key: &'static str,
) -> impl for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static {
    move |client: &Client| admit(&dh, &allow, client, global, key)
}

#[cfg(test)]
mod tests {
    use super::{admissible, exe_of_pid};
    use std::path::PathBuf;

    #[test]
    fn a_deleted_or_relative_exe_is_not_admissible() {
        assert_eq!(
            admissible(PathBuf::from("/usr/bin/grim")),
            Some(PathBuf::from("/usr/bin/grim"))
        );
        assert_eq!(admissible(PathBuf::from("/usr/bin/grim (deleted)")), None);
        assert_eq!(admissible(PathBuf::from("grim")), None);
    }

    #[test]
    fn unusable_pids_deny() {
        assert_eq!(exe_of_pid(0), None);
        assert_eq!(exe_of_pid(-1), None);
    }

    #[test]
    fn this_process_resolves_to_its_own_binary() {
        let me = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(
            exe_of_pid(std::process::id() as i32).as_deref(),
            Some(me.as_path())
        );
    }

    /// A process whose binary is unlinked after it started reads as
    /// `<path> (deleted)` and is denied.
    #[test]
    fn a_process_whose_binary_was_deleted_is_denied() {
        let dir = std::env::temp_dir().join(format!("ec-exe-deleted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("sleeper");
        std::fs::copy("/bin/sleep", &bin).unwrap();
        // ETXTBSY: another test thread's fork can briefly hold the copy's
        // write fd open. Retry rather than flake.
        let mut child = (0..50)
            .find_map(|_| match std::process::Command::new(&bin).arg("5").spawn() {
                Ok(c) => Some(c),
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    None
                }
            })
            .expect("spawn the copied binary");
        let pid = child.id() as i32;
        // Wait for the exec to land: until then /proc shows this binary.
        let bin_c = std::fs::canonicalize(&bin).unwrap();
        for _ in 0..200 {
            if exe_of_pid(pid).as_deref() == Some(bin_c.as_path()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(exe_of_pid(pid).as_deref(), Some(bin_c.as_path()));
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(exe_of_pid(pid), None, "deleted binary must not be admissible");
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
