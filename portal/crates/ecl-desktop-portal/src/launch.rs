// SPDX-License-Identifier: AGPL-3.0-only

//! Start a handler application for a URI or file without `xdg-open`.
//!
//! The target is always its own argv element (never a shell string). When
//! `systemd-run` exists the app gets its own transient scope named per the XDG
//! convention (`app-ecl-<id>-<n>.scope`) so identity resolution can recover its
//! app id; otherwise it is spawned directly in its own process group.

use std::process::Stdio;

use ecl_portal_core::openuri::expand_exec;
use tokio::process::Command;

use crate::apps;

fn scope_unit(desktop_id: &str, n: u64) -> String {
    let id = desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id);
    format!("app-ecl-{}-{n}.scope", id.replace('-', "\\x2d"))
}

/// Launch the first candidate that expands; returns whether one started.
pub async fn open(candidates: &[String], uri: &str, path: Option<&str>, n: u64) -> bool {
    for id in candidates {
        let Some(entry) = apps::desktop_entry(id) else {
            continue;
        };
        if entry.terminal {
            continue;
        }
        let Some(argv) = entry
            .exec
            .as_deref()
            .and_then(|e| expand_exec(e, uri, path))
        else {
            continue;
        };
        if run(id, argv, n).await {
            return true;
        }
    }
    false
}

async fn run(desktop_id: &str, argv: Vec<String>, n: u64) -> bool {
    let (program, rest) = match argv.split_first() {
        Some(p) => p,
        None => return false,
    };
    let via_systemd = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            &format!("--unit={}", scope_unit(desktop_id, n)),
            "--",
        ])
        .arg(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(false)
        .spawn();
    let child = match via_systemd {
        Ok(c) => Some(c),
        Err(_) => Command::new(program)
            .args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(false)
            .spawn()
            .ok(),
    };
    match child {
        Some(mut c) => {
            // Reap it when it ends; the app outlives this call.
            tokio::spawn(async move {
                let _ = c.wait().await;
            });
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_round_trip_through_identity() {
        let u = scope_unit("org.mozilla.firefox.desktop", 7);
        assert_eq!(u, "app-ecl-org.mozilla.firefox-7.scope");
        assert_eq!(
            ecl_portal_core::identity::app_id_from_unit(&u).as_deref(),
            Some("org.mozilla.firefox")
        );
        let d = scope_unit("my-app.desktop", 1);
        assert_eq!(
            ecl_portal_core::identity::app_id_from_unit(&d).as_deref(),
            Some("my-app")
        );
    }
}
