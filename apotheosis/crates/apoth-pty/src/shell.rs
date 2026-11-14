// SPDX-License-Identifier: AGPL-3.0-only
//! Shell-integration snippets (T-14 §2), embedded data assets (T-00 §3.3), each ≤ 8 KiB.
//! Inside an Apotheosis pane (`APOTHEOSIS_PANE` set) they define `ap` as the short form of
//! `apoth` (T-12 §5); nothing is installed system-wide.

/// Largest snippet (T-14 §2).
pub const MAX_SNIPPET_BYTES: usize = 8 * 1024;

/// Shells with a snippet.
pub const SHELLS: &[&str] = &["bash", "zsh", "fish"];

/// The snippet for `shell` (`bash`, `zsh`, `fish`).
#[must_use]
pub fn snippet(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(include_str!("../shell/apotheosis.bash")),
        "zsh" => Some(include_str!("../shell/apotheosis.zsh")),
        "fish" => Some(include_str!("../shell/apotheosis.fish")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn every_snippet_is_bounded_and_defines_ap_in_panes_only() {
        for sh in SHELLS {
            let s = snippet(sh).unwrap();
            assert!(s.len() <= MAX_SNIPPET_BYTES, "{sh}");
            assert!(s.contains("APOTHEOSIS_PANE"), "{sh}: no pane guard");
            let defines = match *sh {
                "fish" => s.contains("function ap ") && s.contains("apoth $argv"),
                _ => s.contains("alias ap='apoth'"),
            };
            assert!(defines, "{sh}: no `ap`");
        }
        assert_eq!(snippet("tcsh"), None);
    }

    /// Run `shell -c` with the snippet sourced and report what `ap` is. `None` when the
    /// shell is not installed (the static test above still covers the snippet).
    fn ap_after_sourcing(shell: &str, pane: bool) -> Option<String> {
        let dir =
            std::env::temp_dir().join(format!("apoth-snippet-{}-{shell}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let file = dir.join("snippet");
        std::fs::write(&file, snippet(shell)?).ok()?;
        let probe = match shell {
            "fish" => format!(
                "source {}; functions -q ap; and echo defined; or echo none",
                file.display()
            ),
            _ => format!(
                "source {}; if alias ap >/dev/null 2>&1; then echo defined; else echo none; fi",
                file.display()
            ),
        };
        let mut c = Command::new(shell);
        if shell == "zsh" {
            c.arg("-f");
        }
        if shell == "bash" {
            c.arg("--norc").arg("--noprofile");
        }
        c.arg("-c").arg(probe).env_remove("APOTHEOSIS_PANE");
        if pane {
            c.env("APOTHEOSIS_PANE", "1");
        }
        let out = c.output().ok()?;
        let _ = std::fs::remove_dir_all(&dir);
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    #[test]
    fn installed_shells_define_ap_only_in_a_pane() {
        for sh in SHELLS {
            if let Some(got) = ap_after_sourcing(sh, true) {
                assert_eq!(got, "defined", "{sh} in a pane");
                assert_eq!(
                    ap_after_sourcing(sh, false).as_deref(),
                    Some("none"),
                    "{sh} outside"
                );
            }
        }
    }
}
