// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop's icons: `~/Desktop` on a `Layer::Bottom` surface per
//! output, the way Plasma's folder view draws it (C-24, ADR 0090).
//!
//! An ordinary layer-shell client outside the TCB. The model modules are
//! non-visual and carry the tests; [`view`] only draws them.

pub mod app;
pub mod desktop;
pub mod grid;
pub mod icons;
pub mod launch;
pub mod layout;
pub mod mime;
pub mod ops;
#[cfg(debug_assertions)]
pub mod preview;
pub mod scan;
pub mod trash;
pub mod ui;
pub mod view;

/// A file or directory name the desktop may create or rename to: non-empty,
/// no path separator or NUL, not `.` or `..`, at most 255 bytes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && name.len() <= 255 && !name.contains(['/', '\0'])
}

/// A fresh scratch directory for a test.
#[cfg(test)]
pub(crate) fn tmp(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "ec-desktop-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("tmp");
    d
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    /// The unit abyss starts under `mode "de"` must never be enabled into the
    /// session target (`ec-abyss/src/session.rs` owns its lifecycle).
    #[test]
    fn the_unit_has_no_install_section() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packaging/ec-desktop.service"
        );
        let text = std::fs::read_to_string(path).expect("packaging/ec-desktop.service");
        assert!(
            !text.lines().any(|l| l.trim() == "[Install]"),
            "abyss starts this unit, nothing else may"
        );
        assert!(text.contains("ExecStart=/usr/bin/ec-desktop"));
    }

    #[test]
    fn names_are_one_path_component() {
        assert!(valid_name("notes.txt"));
        assert!(valid_name(".hidden"));
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(!valid_name(&"x".repeat(256)));
    }
}
