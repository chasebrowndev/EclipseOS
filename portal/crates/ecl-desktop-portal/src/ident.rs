// SPDX-License-Identifier: AGPL-3.0-only

//! Gather the facts about a D-Bus caller and hand them to the pure resolver.
//! Everything here is best effort: any failure degrades to `host`.

use std::fs;

use ecl_portal_core::identity::{host, resolve, Facts};
use ecl_portal_proto::Requester;
use zbus::names::BusName;
use zbus::Connection;

use crate::state::{lock, Shared};

async fn pid_of(conn: &Connection, sender: &str) -> Option<u32> {
    let proxy = zbus::fdo::DBusProxy::new(conn).await.ok()?;
    let name = BusName::try_from(sender).ok()?;
    proxy
        .get_connection_credentials(name)
        .await
        .ok()?
        .process_id()
}

/// Identity to show for `sender`. Advisory only (ADR 0096): never an
/// enforcement key.
pub async fn requester(shared: &Shared, conn: &Connection, sender: &str) -> Requester {
    let Some(pid) = pid_of(conn, sender).await else {
        return host();
    };
    let flatpak = fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")).ok();
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok();
    let exe = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| {
            fs::read_to_string(format!("/proc/{pid}/comm"))
                .ok()
                .map(|s| s.trim().to_owned())
        });
    let facts = Facts {
        flatpak_info: flatpak.as_deref(),
        // No wp_security_context tag reaches a D-Bus peer today.
        security_context: None,
        unit: None,
        cgroup: cgroup.as_deref(),
        exe_name: exe.as_deref(),
    };
    let index = lock(&shared.index);
    resolve(&facts, &index)
}
