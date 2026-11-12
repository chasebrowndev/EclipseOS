// SPDX-License-Identifier: AGPL-3.0-only
//! The closed plan language between `ec-setup` and `ec-setup-helper`
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

/// The smallest target disk: smaller cannot hold the package set plus a usable
/// home. The helper refuses below it; the wizard uses it to refuse early.
pub const MIN_DISK_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// COMP-17 §2.1. A seed, never a runtime layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Minimal,
    Standard,
    Full,
    Agentic,
}

impl Profile {
    /// D-07 §4.4 "Stack": whether the `eclipseos-agents` add-on (ADR 0069) is
    /// preselected. On for Agentic only; the user can change it in step 12.
    pub fn agents_default(self) -> bool {
        matches!(self, Profile::Agentic)
    }
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
    /// Install the agent stack add-on (D-07 §4.4 "Stack", ADR 0069). The UI
    /// seeds it from [`Profile::agents_default`]; absent on the wire means no.
    #[serde(default)]
    pub agents: bool,
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

/// Lowercase `[a-z0-9-]` form of one DMI string, at most `max` bytes; `None`
/// for empty, vendor-placeholder or digit-only values.
fn dmi_slug(raw: &str, max: usize) -> Option<String> {
    const JUNK: [&str; 9] = [
        "to be filled",
        "system product name",
        "system manufacturer",
        "default string",
        "not specified",
        "not applicable",
        "type1productconfigid",
        "standard pc",
        "none",
    ];
    let low = raw.trim().to_ascii_lowercase();
    if low.is_empty() || JUNK.iter().any(|j| low.starts_with(j)) {
        return None;
    }
    let mut s = String::new();
    for c in low.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
    }
    s.truncate(max);
    let s = s.trim_end_matches('-').to_owned();
    (s.len() >= 2 && s.bytes().any(|b| b.is_ascii_alphabetic())).then_some(s)
}

/// The machine's model as a hostname part, from the firmware's DMI strings:
/// `product_family`, else `product_name` (`thinkpad-x1-carbon-gen-11`). A
/// generic family (`Laptop`) gets the vendor in front (`framework-laptop`);
/// with nothing but placeholders, the vendor alone (`asus`). `None` when even
/// that is unusable, so callers fall back.
pub fn model_slug(vendor: &str, models: &[&str]) -> Option<String> {
    const GENERIC: [&str; 7] = [
        "laptop",
        "notebook",
        "desktop",
        "computer",
        "pc",
        "tower",
        "all-series",
    ];
    // Placeholder check on the whole string, then the first word: `Dell Inc.`
    // is `dell`, `System manufacturer` is nothing.
    let vendor =
        dmi_slug(vendor, 64).and_then(|_| dmi_slug(vendor.split_whitespace().next().unwrap_or(""), 12));
    let model = models.iter().find_map(|m| dmi_slug(m, 24));
    match (model, vendor) {
        (Some(m), Some(v)) if GENERIC.contains(&m.as_str()) => Some(format!("{v}-{m}")),
        (Some(m), _) => Some(m),
        (None, v) => v,
    }
}

/// The installer's default hostname, unique enough that two EclipseOS
/// machines on one network or tailnet don't collide: `<user>-<model>`, or
/// `eclipse-<suffix>` (a few random hex digits) without a usable model or
/// before a username is typed. Always a [`valid_hostname`].
pub fn suggest_hostname(username: &str, model: Option<&str>, suffix: &str) -> String {
    let user: String = username
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(32)
        .collect();
    let user = user.trim_matches('-');
    let pick = match model {
        Some(m) if !user.is_empty() => format!("{user}-{m}"),
        _ => format!("eclipse-{suffix}"),
    };
    if valid_hostname(&pick) {
        pick
    } else {
        "eclipse".to_owned()
    }
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
    fn model_slug_cleans_dmi_and_drops_placeholders() {
        // Real machines (2026-11-12): a Framework 13 and an ASUS desktop.
        assert_eq!(
            model_slug("Framework", &["Laptop", "Laptop 13 (AMD Ryzen AI 300 Series)"]).as_deref(),
            Some("framework-laptop")
        );
        assert_eq!(
            model_slug("ASUS", &["To be filled by O.E.M.", "System Product Name"]).as_deref(),
            Some("asus")
        );
        assert_eq!(
            model_slug("LENOVO", &["ThinkPad X1 Carbon Gen 11", ""]).as_deref(),
            Some("thinkpad-x1-carbon-gen-1")
        );
        assert_eq!(
            model_slug("Dell Inc.", &["", "XPS 13 9340"]).as_deref(),
            Some("xps-13-9340")
        );
        assert_eq!(model_slug("System manufacturer", &["Default string"]), None);
        assert_eq!(
            model_slug("QEMU", &["Standard PC (Q35 + ICH9, 2009)"]).as_deref(),
            Some("qemu")
        );
        assert_eq!(model_slug("", &["123"]), None);
    }

    #[test]
    fn suggested_hostnames_are_valid_and_personal() {
        assert_eq!(
            suggest_hostname("chase", Some("framework"), "7f3a"),
            "chase-framework"
        );
        assert_eq!(suggest_hostname("", Some("framework"), "7f3a"), "eclipse-7f3a");
        assert_eq!(suggest_hostname("chase", None, "7f3a"), "eclipse-7f3a");
        assert_eq!(suggest_hostname("Bad_User", Some("x1"), "00ff"), "adser-x1");
        for (u, m, s) in [
            ("-", Some("m"), "ab12"),
            ("a".repeat(40).as_str(), Some("model"), "ab12"),
            ("x", None, ""),
        ] {
            assert!(valid_hostname(&suggest_hostname(u, m, s)), "{u:?} {m:?} {s:?}");
        }
    }

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
            candidates: vec!["ec-hyperion-bar".into()],
            agents: false,
        };
        let j = serde_json::to_string(&plan).unwrap();
        assert!(!j.contains("password"));
        assert_eq!(serde_json::from_str::<Plan>(&j).unwrap(), plan);
    }

    #[test]
    fn agents_default_on_for_agentic_only_and_absent_is_off() {
        assert!(Profile::Agentic.agents_default());
        for p in [Profile::Minimal, Profile::Standard, Profile::Full] {
            assert!(!p.agents_default(), "{p:?}");
        }
        // A plan from before the field existed still parses, as "no agents".
        let old = r#"{"disk_by_id":"d","hostname":"h","username":"u","locale":"en_US.UTF-8",
            "timezone":"UTC","keymap":"us","carry_network":true,"profile":"agentic","candidates":[]}"#;
        assert!(!serde_json::from_str::<Plan>(old).unwrap().agents);
    }
}
