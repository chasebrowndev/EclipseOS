// SPDX-License-Identifier: AGPL-3.0-only
//! Carrying the live medium's Wi-Fi connections to the target (D-07 §4.3). On
//! unless the user cleared it on Review. Best effort: whatever cannot be read
//! or does not look like a plain system Wi-Fi keyfile is skipped, and a skip
//! never fails the install. File contents (they hold the passphrase) are never
//! logged or reported; only a count leaves this module.

use crate::env::Env;
use crate::target;
use std::fs;
use std::os::unix::fs::MetadataExt;

const MAX_FILES: usize = 32;
const MAX_BYTES: u64 = 64 * 1024;

/// A keyfile for a Wi-Fi connection that every user may use. Private
/// connections (`permissions=user:…`) belong to the live user and would be
/// dead weight on the target.
fn portable_wifi(text: &str) -> bool {
    let mut wifi = false;
    for l in text.lines() {
        let l = l.trim();
        if l == "type=wifi" {
            wifi = true;
        } else if l.strip_prefix("permissions=").is_some_and(|v| !v.is_empty()) {
            return false;
        }
    }
    wifi
}

/// Returns how many connections were copied.
pub fn carry(env: &Env) -> usize {
    let p = &env.paths;
    let Ok(rd) = fs::read_dir(&p.nm_connections) else {
        return 0;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| target::valid_nm_name(n))
        .collect();
    names.sort();
    let mut n = 0;
    for name in names.into_iter().take(MAX_FILES) {
        let path = p.nm_connections.join(&name);
        // Regular file, root-owned, not readable by anyone else: how NetworkManager
        // writes them. A link or a loose file is not ours to trust.
        let Ok(md) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !md.is_file() || md.uid() != 0 || md.mode() & 0o077 != 0 || md.len() > MAX_BYTES {
            continue;
        }
        let Ok(bytes) = target::read_bounded(&path) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !portable_wifi(text) {
            continue;
        }
        if target::write_nm_connection(&p.target, &name, &bytes).is_ok() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::DenyConfirm;
    use crate::testutil::{live, FakeRunner};
    use std::os::unix::fs::PermissionsExt;

    const HOME: &str = "[connection]\nid=home\ntype=wifi\n\n[wifi-security]\npsk=hunter2hunter2\n";

    fn put(dir: &std::path::Path, name: &str, body: &str, mode: u32) {
        fs::create_dir_all(dir).unwrap();
        let f = dir.join(name);
        fs::write(&f, body).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn what_counts_as_portable_wifi() {
        assert!(portable_wifi(HOME));
        assert!(portable_wifi("type=wifi\npermissions=\n"));
        assert!(!portable_wifi("[connection]\ntype=ethernet\n"));
        assert!(!portable_wifi("type=wifi\npermissions=user:liveuser:;\n"));
        assert!(!portable_wifi(""));
    }

    #[test]
    fn copies_only_root_only_wifi_keyfiles_with_mode_600() {
        // Files here are owned by the test's uid, not 0, so this exercises the
        // owner check's refusal; the copy path is covered by `portable_wifi`
        // plus the target tests, and end to end by the VM run.
        let (_t, p) = live();
        put(&p.nm_connections, "home.nmconnection", HOME, 0o600);
        let r = FakeRunner::new(|_| Ok(Vec::new()));
        let env = Env {
            paths: p,
            euid: 0,
            caller_uid_hint: None,
            runner: &r,
            confirm: &DenyConfirm,
        };
        let is_root = rustix::process::getuid().is_root();
        assert_eq!(carry(&env), usize::from(is_root));
    }

    #[test]
    fn loose_links_and_odd_files_are_skipped() {
        let (_t, p) = live();
        put(&p.nm_connections, "open.nmconnection", HOME, 0o644);
        put(&p.nm_connections, "notes.txt", HOME, 0o600);
        std::os::unix::fs::symlink("/etc/shadow", p.nm_connections.join("link.nmconnection")).unwrap();
        let r = FakeRunner::new(|_| Ok(Vec::new()));
        let target = p.target.clone();
        let env = Env {
            paths: p,
            euid: 0,
            caller_uid_hint: None,
            runner: &r,
            confirm: &DenyConfirm,
        };
        assert_eq!(carry(&env), 0);
        assert!(!target.join(target::NM_DIR).exists());
    }

    #[test]
    fn no_connection_directory_is_zero() {
        let (_t, p) = live();
        let r = FakeRunner::new(|_| Ok(Vec::new()));
        let env = Env {
            paths: p,
            euid: 0,
            caller_uid_hint: None,
            runner: &r,
            confirm: &DenyConfirm,
        };
        assert_eq!(carry(&env), 0);
    }
}
