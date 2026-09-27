// SPDX-License-Identifier: AGPL-3.0-only
//! The closed plan language between `eclipse-setup` and `eclipse-setup-helper`
//! (D-07 §6). The helper's input is this and nothing else: no shell command, no
//! path, no package name, no unit name. Types and validators only; no I/O, so
//! the GUI and the root helper can share one definition without sharing code
//! that touches the system.
//!
//! Wire format: one JSON object on the helper's stdin, [`Request`], then one
//! JSON [`Progress`] per line on stdout. The password is not part of [`Plan`]:
//! it rides as its own zeroizing field of the frame, so nothing that logs or
//! serialises a plan can carry it (root invariant 6).

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// COMP-17 §2.1. A seed, never a runtime layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Minimal,
    Standard,
    Full,
    Agentic,
}

/// Everything the helper acts on, all validated by the helper itself (D-07
/// §4.2, §4.3, §6). Nothing here is trusted because the UI validated it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// A `/dev/disk/by-id` name from the helper's own listing, never a free path.
    pub disk_by_id: String,
    pub hostname: String,
    pub username: String,
    /// e.g. `en_US.UTF-8`, from the locale list.
    pub locale: String,
    /// e.g. `America/New_York`, from the tzdata list.
    pub timezone: String,
    /// Console keymap for `/etc/vconsole.conf`, e.g. `de-latin1`. The helper
    /// checks it against the installed kbd keymaps. The graphical layout rides
    /// in the seed (`input.kb-layout`).
    pub keymap: String,
    /// Copy the live NetworkManager connections to the target (D-07 §4.3). On
    /// by default in the UI; the user can clear it on Review.
    pub carry_network: bool,
    pub profile: Profile,
    /// Catalog ids only. The helper resolves them; it takes no package or unit
    /// name from its caller.
    pub candidates: Vec<String>,
}

/// The stdin frame: the plan plus the one secret.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub plan: Plan,
    /// Cleared on drop. Rejected by [`check_password`] if it holds `\n`, `\r` or NUL.
    pub password: Zeroizing<String>,
}

/// Install stages in the order the helper runs them. Everything up to and
/// including `Confirm` can fail without touching the disk (D-07 §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Validate,
    Preflight,
    Confirm,
    Partition,
    Pacstrap,
    Configure,
    Bootloader,
    User,
    Seed,
    Units,
    Done,
}

/// One line of helper stdout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: Stage,
    /// 0..=100 within the whole install.
    pub pct: u8,
    /// Short and fixed-vocabulary. Never echoes file content or the password.
    pub msg: String,
    /// Set on the first failed stage; the run stops there.
    pub failed: bool,
}

/// A row of the helper's own disk listing (D-07 §4.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub by_id: String,
    pub model: String,
    pub size_bytes: u64,
    pub partitions: Vec<Partition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub fs: String,
    pub label: String,
    pub size_bytes: u64,
}

/// RFC 1123 label: what `useradd`/`hostnamectl` accept without surprises.
pub fn valid_hostname(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=63).contains(&b.len())
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// A conservative POSIX login name, and never `root`.
pub fn valid_username(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
        && s != "root"
}

/// Root is locked, so this is also the sudo password.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// `chpasswd` reads `user:password` lines, so a newline would inject a second
/// entry (D-07 §6). Shorter than [`MIN_PASSWORD_CHARS`] is refused.
pub fn check_password(s: &str) -> bool {
    s.chars().count() >= MIN_PASSWORD_CHARS && !s.bytes().any(|c| matches!(c, b'\n' | b'\r' | 0))
}

/// A kbd keymap name: one path component, as `loadkeys` and `vconsole.conf` take it.
pub fn valid_keymap(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

/// A `/dev/disk/by-id` entry name: one path component, no separators.
pub fn valid_by_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with('.')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':' | b'+' | b'='))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames() {
        assert!(valid_hostname("eclipse-1"));
        for bad in ["", "-a", "a-", "A", "a_b", "a b", "a.b", &"a".repeat(64)] {
            assert!(!valid_hostname(bad), "{bad:?}");
        }
    }

    #[test]
    fn usernames() {
        assert!(valid_username("chase"));
        assert!(valid_username("_svc-1"));
        for bad in ["", "root", "1a", "Chase", "a b", "a:b", "a\n", &"a".repeat(33)] {
            assert!(!valid_username(bad), "{bad:?}");
        }
    }

    #[test]
    fn passwords() {
        assert!(check_password("correct horse"));
        assert!(check_password("üüüüüüüü"));
        for bad in ["", "short", "a\nroot:x1234", "abcdefg\r", "abc\0defgh"] {
            assert!(!check_password(bad), "{bad:?}");
        }
    }

    #[test]
    fn keymaps() {
        for ok in ["us", "de-latin1", "uk", "br-abnt2", "cz-qwertz"] {
            assert!(valid_keymap(ok), "{ok:?}");
        }
        for bad in ["", "-us", ".us", "../us", "a/b", "a b", "a\n", &"a".repeat(65)] {
            assert!(!valid_keymap(bad), "{bad:?}");
        }
    }

    #[test]
    fn by_id_is_one_component() {
        assert!(valid_by_id("nvme-Samsung_SSD_970_EVO_1TB_S4EWNX0M123456"));
        for bad in ["", "../sda", "a/b", ".hidden", "a b", "a\0b"] {
            assert!(!valid_by_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn plan_json_carries_no_password_and_roundtrips() {
        let plan = Plan {
            disk_by_id: "nvme-x".into(),
            hostname: "h".into(),
            username: "u".into(),
            locale: "en_US.UTF-8".into(),
            timezone: "UTC".into(),
            keymap: "us".into(),
            carry_network: true,
            profile: Profile::Standard,
            candidates: vec!["hyperion".into()],
        };
        let j = serde_json::to_string(&plan).unwrap();
        assert!(!j.contains("password"));
        assert_eq!(serde_json::from_str::<Plan>(&j).unwrap(), plan);
    }
}
