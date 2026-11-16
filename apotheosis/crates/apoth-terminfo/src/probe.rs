// SPDX-License-Identifier: Apache-2.0
//! The terminfo database probe and the child environment (T-19 §1).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Variant;

/// The `TERM` fallback when our entry is not installed.
pub const FALLBACK_TERM: &str = "xterm-256color";

/// The system directories of T-19 §1, in probe order.
pub const SYSTEM_DIRS: [&str; 3] = ["/etc/terminfo", "/lib/terminfo", "/usr/share/terminfo"];

/// The outcome of [`probe`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermChoice {
    /// The `TERM` value to export to the child.
    pub term: String,
    /// The directory the entry was found in (`None` under the fallback).
    pub found_in: Option<PathBuf>,
}

fn has_entry(dir: &Path, name: &str) -> bool {
    let first = name.as_bytes()[0];
    [char::from(first).to_string(), format!("{first:02x}")]
        .iter()
        .any(|d| dir.join(d).join(name).is_file())
}

/// Probe order: `$TERMINFO`, `~/.terminfo`, each `$TERMINFO_DIRS` entry (an empty entry
/// means the system directories), then `system_dirs`. Each directory is tried with the
/// letter and the hex subdirectory. `env` reads the process environment (injected so the
/// probe is testable). Returns `variant`'s name if found, else [`FALLBACK_TERM`].
pub fn probe(
    env: &dyn Fn(&str) -> Option<OsString>,
    system_dirs: &[PathBuf],
    variant: Variant,
) -> TermChoice {
    let name = variant.term_name();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(t) = env("TERMINFO").filter(|t| !t.is_empty()) {
        dirs.push(t.into());
    }
    if let Some(h) = env("HOME").filter(|h| !h.is_empty()) {
        dirs.push(PathBuf::from(h).join(".terminfo"));
    }
    if let Some(list) = env("TERMINFO_DIRS") {
        for d in std::env::split_paths(&list) {
            if d.as_os_str().is_empty() {
                dirs.extend(system_dirs.iter().cloned());
            } else {
                dirs.push(d);
            }
        }
    }
    dirs.extend(system_dirs.iter().cloned());
    for d in dirs {
        if has_entry(&d, name) {
            return TermChoice {
                term: name.to_owned(),
                found_in: Some(d),
            };
        }
    }
    TermChoice {
        term: FALLBACK_TERM.to_owned(),
        found_in: None,
    }
}

/// [`probe`] against the real environment and [`SYSTEM_DIRS`].
pub fn probe_system(variant: Variant) -> TermChoice {
    let dirs: Vec<PathBuf> = SYSTEM_DIRS.iter().map(PathBuf::from).collect();
    probe(&|k| std::env::var_os(k), &dirs, variant)
}

/// The identity variables exported to the child (T-19 §1); `APOTHEOSIS_*` is T-12 §3's.
pub fn child_env(choice: &TermChoice, version: &str) -> Vec<(&'static str, String)> {
    vec![
        ("TERM", choice.term.clone()),
        ("COLORTERM", "truecolor".to_owned()),
        ("TERM_PROGRAM", "Apotheosis".to_owned()),
        ("TERM_PROGRAM_VERSION", version.to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apoth-probe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, sub: &str) {
        let p = root.join(sub).join("xterm-apotheosis");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn order_and_layouts() {
        let root = tmp("order");
        let (ti, home, extra, sys) = (
            root.join("ti"),
            root.join("home"),
            root.join("extra"),
            root.join("sys"),
        );
        let sysv = vec![sys.clone()];
        let env = |k: &str| -> Option<OsString> {
            match k {
                "TERMINFO" => Some(ti.clone().into()),
                "HOME" => Some(home.clone().into()),
                "TERMINFO_DIRS" => Some(extra.clone().into()),
                _ => None,
            }
        };
        let got = |_: ()| probe(&env, &sysv, Variant::Indexed);
        assert_eq!(got(()).term, "xterm-256color");
        assert_eq!(got(()).found_in, None);
        put(&sys, "78"); // hex layout in the system dir
        assert_eq!(got(()).found_in, Some(sys.clone()));
        put(&extra, "x");
        assert_eq!(got(()).found_in, Some(extra.clone()));
        put(&home.join(".terminfo"), "x");
        assert_eq!(got(()).found_in, Some(home.join(".terminfo")));
        put(&ti, "78");
        assert_eq!(got(()).found_in, Some(ti.clone()));
        assert_eq!(got(()).term, "xterm-apotheosis");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn empty_terminfo_dirs_entry_means_system() {
        let root = tmp("empty");
        let sys = root.join("sys");
        put(&sys, "x");
        let env = |k: &str| (k == "TERMINFO_DIRS").then(|| OsString::from(":"));
        let c = probe(&env, std::slice::from_ref(&sys), Variant::Indexed);
        assert_eq!(c.found_in, Some(sys));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn direct_variant_not_confused() {
        let root = tmp("direct");
        let sys = root.join("sys");
        put(&sys, "x"); // only xterm-apotheosis
        let c = probe(&|_| None, std::slice::from_ref(&sys), Variant::Direct);
        assert_eq!(c.term, FALLBACK_TERM);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn child_env_values() {
        let c = TermChoice {
            term: FALLBACK_TERM.into(),
            found_in: None,
        };
        let e = child_env(&c, "0.1.0");
        assert!(e.contains(&("TERM", "xterm-256color".to_owned())));
        assert!(e.contains(&("COLORTERM", "truecolor".to_owned())));
        assert!(e.contains(&("TERM_PROGRAM", "Apotheosis".to_owned())));
        assert!(e.contains(&("TERM_PROGRAM_VERSION", "0.1.0".to_owned())));
    }
}
