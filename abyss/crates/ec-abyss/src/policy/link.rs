// SPDX-License-Identifier: AGPL-3.0-only
//! The compositor's end of `policyd.sock`: where the grant verifying key
//! comes from (F-05, COMP-01 §6). TCB.
//!
//! abyss dials `policyd`, and trusts the far end only if the kernel says it
//! is the session user's `policyd.service`: the `SO_PEERCRED` uid is ours
//! and the peer pid's cgroup leaf is `policyd.service`. The first key offered
//! is pinned for the life of the compositor; a different key on a later
//! connection is refused and the link stays down.
//!
//! While the link is down, `state.policy_key` is `None` and every agent is
//! paused (S-01 §6, F-08). It is redialled every [`RETRY`] while the agents
//! hook is on.
//!
//! Once the key is pinned, every further packet is a link message
//! (`ec_policy_eval::link::FromPolicyd`, v2), handed to its owner by
//! [`deliver`]. A packet that does not decode drops the link: a peer whose
//! stream cannot be trusted is cut off, not partly believed. [`send`] puts a
//! message on the audit queue, so it keeps its place among the records.

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::time::Duration;

use ec_policy_eval::link::{decode_key_offer, FromPolicyd, ToPolicyd, MAX_MESSAGE};
use ec_policy_eval::VerifyingKey;
use rustix::net::{self, AddressFamily, RecvFlags, SocketAddrUnix, SocketFlags, SocketType};
use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, Mode, PostAction,
};

use crate::addons::Hook;
use crate::state::AbyssState;

const RETRY: Duration = Duration::from_secs(1);

/// The systemd unit `policyd` must run as (`dist/policyd.service`).
const UNIT: &str = "policyd.service";

/// The link's state. `state.policy_key` is the key while the link is up.
#[derive(Debug, Default)]
pub struct Link {
    /// The key first offered this session (F-05: pinned, never replaced).
    pinned: Option<VerifyingKey>,
    /// A connection exists (awaiting its offer or up), or a redial is armed.
    busy: bool,
    /// A changed key was offered; nothing more is dialled this session.
    refused: bool,
    /// Receive buffer, [`MAX_MESSAGE`] once the link has been up: a table
    /// does not fit on the stack.
    buf: Vec<u8>,
}

/// Send `m` to `policyd`, in order with the audit stream. While the link is
/// down it waits in the queue; a caller that needs an answer in bounded time
/// (a deferral) runs its own timer and fails closed.
pub fn send(state: &mut AbyssState, m: &ToPolicyd) {
    crate::audit::message(state, m.encode());
}

/// Hand a decoded message to the part of abyss that owns it.
fn deliver(state: &mut AbyssState, m: FromPolicyd) {
    match m {
        FromPolicyd::Table { table, sig } => crate::policy::table::receive(state, &table, &sig),
        FromPolicyd::Revoked { principal } => crate::policy::lifecycle::revoked(state, &principal),
        FromPolicyd::DeferAnswer { req, answer } => crate::policy::enforce::defer_answer(state, req, answer),
        FromPolicyd::Minted { req, grant } => crate::policy::enforce::minted(state, req, Some(grant)),
        FromPolicyd::MintRefused { req } => crate::policy::enforce::minted(state, req, None),
        FromPolicyd::AuditRecords { req, records } => {
            crate::trusted_ui::panel::audit_records(state, req, records)
        }
        // Answers to messages this build does not send yet (the console
        // wave), and agentd's pushes. Ignored, not believed: a grant reaches
        // an agent only through admission.
        FromPolicyd::TaskOpened { .. }
        | FromPolicyd::TaskRefused { .. }
        | FromPolicyd::Preview { .. }
        | FromPolicyd::TaskCreated { .. }
        | FromPolicyd::InstallReview { .. }
        | FromPolicyd::Done { .. }
        | FromPolicyd::Refused { .. }
        | FromPolicyd::TaskState { .. }
        | FromPolicyd::Provision { .. } => {}
    }
}

fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ECLIPSE_POLICYD_SOCKET") {
        return Some(PathBuf::from(p));
    }
    Some(crate::ipc::socket_dir()?.join("policyd.sock"))
}

/// Starts the link if the agents hook is on and it is not already running.
pub fn start(state: &mut AbyssState) {
    if state.policy_link.busy || state.policy_link.refused || !state.addons.hooks.is_on(Hook::Agents) {
        return;
    }
    if !dial(state) {
        arm_retry(state);
    }
}

fn arm_retry(state: &mut AbyssState) {
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(RETRY), |_, _, state| {
            if state.policy_link.refused || !state.addons.hooks.is_on(Hook::Agents) {
                state.policy_link.busy = false;
                return TimeoutAction::Drop;
            }
            if dial(state) {
                TimeoutAction::Drop
            } else {
                TimeoutAction::ToDuration(RETRY)
            }
        });
    match armed {
        Ok(_) => state.policy_link.busy = true,
        Err(e) => tracing::warn!(%e, "arming the policyd redial"),
    }
}

/// One connection attempt. True when a connection is now in the loop.
fn dial(state: &mut AbyssState) -> bool {
    let Some(path) = socket_path() else {
        return false;
    };
    let Ok(fd) = connect(&path) else {
        return false;
    };
    if let Err(why) = authenticate(&fd) {
        tracing::warn!(path = %path.display(), why, "policyd.sock peer is not policyd; not trusting it");
        return false;
    }
    let inserted =
        state
            .loop_handle
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, fd, state| {
                Ok(if readable(state, fd) {
                    PostAction::Continue
                } else {
                    down(state);
                    PostAction::Remove
                })
            });
    match inserted {
        Ok(_) => {
            state.policy_link.busy = true;
            true
        }
        Err(e) => {
            tracing::warn!(%e, "adding the policyd link to the loop");
            false
        }
    }
}

fn connect(path: &std::path::Path) -> rustix::io::Result<OwnedFd> {
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    net::connect(&fd, &SocketAddrUnix::new(path)?)?;
    Ok(fd)
}

/// The F-05 peer check, from what the kernel recorded when `policyd` bound
/// the socket. Neither input is under the peer's control.
fn authenticate(fd: &OwnedFd) -> Result<(), &'static str> {
    let cred = net::sockopt::socket_peercred(fd).map_err(|_| "no SO_PEERCRED")?;
    if cred.uid != rustix::process::getuid() {
        return Err("peer uid is not the session uid");
    }
    let cgroup = std::fs::read_to_string(format!("/proc/{}/cgroup", cred.pid.as_raw_pid()))
        .map_err(|_| "peer cgroup unreadable")?;
    if !cgroup_is_policyd(&cgroup) {
        return Err("peer is not in policyd.service");
    }
    Ok(())
}

/// Whether `/proc/<pid>/cgroup` (cgroup v2: one `0::<path>` line) places the
/// process in a unit named exactly [`UNIT`].
fn cgroup_is_policyd(cgroup: &str) -> bool {
    let mut lines = cgroup.lines();
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return false;
    };
    line.strip_prefix("0::")
        .and_then(|p| p.rsplit('/').next())
        .is_some_and(|leaf| leaf == UNIT)
}

/// What a newly offered key means for the session's pin.
#[derive(Debug, PartialEq, Eq)]
enum Pin {
    /// First key this session, or the pinned one again.
    Accept,
    /// Not the pinned key (F-05: refused).
    Refuse,
}

fn pin(pinned: &mut Option<VerifyingKey>, offered: VerifyingKey) -> Pin {
    match pinned {
        None => {
            *pinned = Some(offered);
            Pin::Accept
        }
        Some(k) if *k == offered => Pin::Accept,
        Some(_) => Pin::Refuse,
    }
}

/// Handles one readable event. False when the link must come down.
fn readable(state: &mut AbyssState, fd: &OwnedFd) -> bool {
    let mut buf = std::mem::take(&mut state.policy_link.buf);
    buf.resize(MAX_MESSAGE, 0);
    let ok = readable_into(state, fd, &mut buf);
    state.policy_link.buf = buf;
    ok
}

fn readable_into(state: &mut AbyssState, fd: &OwnedFd, buf: &mut [u8]) -> bool {
    let n = match net::recv(fd, &mut *buf, RecvFlags::DONTWAIT | RecvFlags::TRUNC) {
        // With TRUNC the second value is the message's real length.
        Ok((n, full)) if n == full && n > 0 => n,
        Err(rustix::io::Errno::AGAIN) => return true,
        Err(rustix::io::Errno::INTR) => return true,
        // EOF, error, or a message too large to be anything we speak.
        _ => return false,
    };
    if state.policy_key.is_some() {
        return match FromPolicyd::decode(&buf[..n]) {
            Ok(m) => {
                deliver(state, m);
                true
            }
            Err(e) => {
                tracing::warn!(?e, "malformed message from policyd; dropping the link");
                false
            }
        };
    }
    let offered = match decode_key_offer(&buf[..n]) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(?e, "policyd key offer malformed; dropping the link");
            return false;
        }
    };
    match pin(&mut state.policy_link.pinned, offered) {
        Pin::Accept => {
            state.policy_key = Some(offered);
            crate::audit::up(state, fd);
            tracing::info!("policyd link up; agents resume");
            true
        }
        Pin::Refuse => {
            // TODO(M13): raise this on the trusted indicator, not only the journal.
            tracing::error!("policyd offered a different key than the one pinned this session; refusing it, agents stay paused");
            state.policy_link.refused = true;
            false
        }
    }
}

fn down(state: &mut AbyssState) {
    crate::audit::down(state);
    if state.policy_key.take().is_some() {
        tracing::warn!("policyd link down; every agent is paused");
    }
    state.policy_link.busy = false;
    if !state.policy_link.refused {
        arm_retry(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn only_a_unit_named_policyd_service_passes() {
        let ok = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/policyd.service\n";
        assert!(cgroup_is_policyd(ok));
        for bad in [
            "",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/evil-policyd.service\n",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/policyd.service/sub\n",
            "0::/user.slice/user-1000.slice/session-2.scope\n",
            "1:name=systemd:/policyd.service\n",
            // A v1 hierarchy line alongside: not the single v2 line we expect.
            "1:cpu:/x\n0::/a/policyd.service\n",
        ] {
            assert!(!cgroup_is_policyd(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_first_key_is_pinned_and_a_changed_one_refused() {
        let a = SigningKey::from_bytes(&[1; 32]).verifying_key();
        let b = SigningKey::from_bytes(&[2; 32]).verifying_key();
        let mut pinned = None;
        assert_eq!(pin(&mut pinned, a), Pin::Accept);
        assert_eq!(pin(&mut pinned, a), Pin::Accept);
        assert_eq!(pin(&mut pinned, b), Pin::Refuse);
        assert_eq!(pinned, Some(a));
    }
}
