// SPDX-License-Identifier: AGPL-3.0-only
//! Child environment construction (T-18 §7, T-19 §1, SHELLINTEGRATION-31).
//!
//! Inherit-then-scrub: the base is the caller's environment, minus other terminals'
//! variables and activation tokens, plus ours. (Spec says "clean base" and also lists a
//! scrub; this is the reading that satisfies both. Reported as an open item.)

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Exact names removed from the inherited environment.
const SCRUB_EXACT: &[&str] = &[
    "XDG_ACTIVATION_TOKEN",
    "DESKTOP_STARTUP_ID",
    "TERMCAP",
    "COLORTERM",
    "COLUMNS",
    "LINES",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "VTE_VERSION",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "WINDOWID",
    "TERMINAL_EMULATOR",
    "TERMINAL_NAME",
    "TERMINAL_VERSION_STRING",
    "TERMINAL_VERSION_STABLE",
];

/// Name prefixes removed (other terminals' families, and stale ours).
const SCRUB_PREFIX: &[&str] = &[
    "KITTY_",
    "WEZTERM_",
    "GHOSTTY_",
    "ALACRITTY_",
    "ITERM_",
    "KONSOLE_",
    "TILIX_",
    "TERMINATOR_",
    "WARP_",
    "FOOT_",
    "APOTHEOSIS_",
];

fn scrubbed(name: &OsStr) -> bool {
    let b = name.as_bytes();
    SCRUB_EXACT.iter().any(|e| e.as_bytes() == b)
        || SCRUB_PREFIX.iter().any(|p| b.starts_with(p.as_bytes()))
}

/// What the terminal itself sets in the child.
#[derive(Debug, Clone)]
pub struct EnvSpec<'a> {
    /// `xterm-apotheosis`, `xterm-apotheosis-direct` or the `xterm-256color` fallback.
    pub term: &'a str,
    /// `TERM_PROGRAM_VERSION`.
    pub version: &'a str,
    /// Applied last: `APOTHEOSIS_SOCKET`, `APOTHEOSIS_PANE`, `APOTHEOSIS_WINDOW`,
    /// `APOTHEOSIS_VERSION`, user `env` config.
    pub extra: Vec<(OsString, OsString)>,
}

/// Build the final child environment (sorted by name, deterministic).
pub fn build_env<I>(base: I, spec: &EnvSpec<'_>) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut m: BTreeMap<OsString, OsString> = base
        .into_iter()
        .filter(|(k, _)| !scrubbed(k) && !k.is_empty())
        .collect();
    m.insert("TERM".into(), spec.term.into());
    m.insert("COLORTERM".into(), "truecolor".into());
    m.insert("TERM_PROGRAM".into(), "Apotheosis".into());
    m.insert("TERM_PROGRAM_VERSION".into(), spec.version.into());
    for (k, v) in &spec.extra {
        m.insert(k.clone(), v.clone());
    }
    m.into_iter().collect()
}

/// Terminfo search order (T-19 §1): `TERMINFO`, `~/.terminfo`, `TERMINFO_DIRS`,
/// `/etc/terminfo`, `/lib/terminfo`, `/usr/share/terminfo`.
pub fn terminfo_dirs(get: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(t) = get("TERMINFO").filter(|t| !t.is_empty()) {
        v.push(PathBuf::from(t));
    }
    if let Some(h) = get("HOME").filter(|h| !h.is_empty()) {
        v.push(Path::new(&h).join(".terminfo"));
    }
    if let Some(d) = get("TERMINFO_DIRS") {
        for part in d.as_bytes().split(|&c| c == b':') {
            if part.is_empty() {
                v.push(PathBuf::from("/usr/share/terminfo"));
            } else {
                v.push(PathBuf::from(OsStr::from_bytes(part)));
            }
        }
    }
    for s in ["/etc/terminfo", "/lib/terminfo", "/usr/share/terminfo"] {
        v.push(PathBuf::from(s));
    }
    v
}

/// Does `<dir>/<first char>/<name>` or `<dir>/<hex of first byte>/<name>` exist in any dir?
pub fn terminfo_exists(name: &str, dirs: &[PathBuf]) -> bool {
    let Some(&first) = name.as_bytes().first() else {
        return false;
    };
    let letter = (first as char).to_string();
    let hex = format!("{first:02x}");
    dirs.iter()
        .any(|d| d.join(&letter).join(name).is_file() || d.join(&hex).join(name).is_file())
}

/// `xterm-apotheosis[-direct]` if its entry is installed, else `xterm-256color`.
pub fn choose_term(direct: bool, dirs: &[PathBuf]) -> &'static str {
    let want = if direct {
        "xterm-apotheosis-direct"
    } else {
        "xterm-apotheosis"
    };
    if terminfo_exists(want, dirs) {
        want
    } else {
        "xterm-256color"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn scrubs_foreign_terminal_vars_and_tokens() {
        let base = [
            ("PATH", "/usr/bin"),
            ("KITTY_WINDOW_ID", "3"),
            ("WEZTERM_PANE", "1"),
            ("GHOSTTY_RESOURCES_DIR", "/x"),
            ("ALACRITTY_LOG", "/y"),
            ("VTE_VERSION", "7000"),
            ("WT_SESSION", "z"),
            ("XDG_ACTIVATION_TOKEN", "tok"),
            ("DESKTOP_STARTUP_ID", "id"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("APOTHEOSIS_PANE", "stale"),
            ("HOME", "/home/u"),
        ]
        .map(|(k, v)| (os(k), os(v)));
        let spec = EnvSpec {
            term: "xterm-apotheosis",
            version: "1.2.3",
            extra: vec![(os("APOTHEOSIS_PANE"), os("7"))],
        };
        let out: BTreeMap<_, _> = build_env(base, &spec).into_iter().collect();
        for gone in [
            "KITTY_WINDOW_ID",
            "WEZTERM_PANE",
            "GHOSTTY_RESOURCES_DIR",
            "ALACRITTY_LOG",
            "VTE_VERSION",
            "WT_SESSION",
            "XDG_ACTIVATION_TOKEN",
            "DESKTOP_STARTUP_ID",
        ] {
            assert!(!out.contains_key(&os(gone)), "{gone} leaked");
        }
        assert_eq!(out[&os("TERM")], os("xterm-apotheosis"));
        assert_eq!(out[&os("COLORTERM")], os("truecolor"));
        assert_eq!(out[&os("TERM_PROGRAM")], os("Apotheosis"));
        assert_eq!(out[&os("TERM_PROGRAM_VERSION")], os("1.2.3"));
        assert_eq!(out[&os("APOTHEOSIS_PANE")], os("7"));
        assert_eq!(out[&os("PATH")], os("/usr/bin"));
        assert_eq!(out[&os("HOME")], os("/home/u"));
    }

    #[test]
    fn terminfo_probe_order() {
        let get = |k: &str| match k {
            "TERMINFO" => Some(os("/t")),
            "HOME" => Some(os("/h")),
            "TERMINFO_DIRS" => Some(os("/a::/b")),
            _ => None,
        };
        let d = terminfo_dirs(&get);
        let s: Vec<_> = d.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            s,
            [
                "/t",
                "/h/.terminfo",
                "/a",
                "/usr/share/terminfo",
                "/b",
                "/etc/terminfo",
                "/lib/terminfo",
                "/usr/share/terminfo"
            ]
        );
    }

    #[test]
    fn choose_term_letter_hex_and_fallback() {
        let base = std::env::temp_dir().join(format!("apoth-pty-ti-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dirs = vec![base.clone()];
        assert_eq!(choose_term(false, &dirs), "xterm-256color");
        std::fs::create_dir_all(base.join("78")).unwrap();
        std::fs::write(base.join("78").join("xterm-apotheosis"), b"x").unwrap();
        assert_eq!(choose_term(false, &dirs), "xterm-apotheosis");
        assert_eq!(choose_term(true, &dirs), "xterm-256color");
        std::fs::create_dir_all(base.join("x")).unwrap();
        std::fs::write(base.join("x").join("xterm-apotheosis-direct"), b"x").unwrap();
        assert_eq!(choose_term(true, &dirs), "xterm-apotheosis-direct");
        let _ = std::fs::remove_dir_all(&base);
    }
}
