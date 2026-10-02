// SPDX-License-Identifier: AGPL-3.0-only
//! The premade command-widget catalog (ADR 0067 "Premade catalog"; COMP-10
//! §3.11).
//!
//! The taskbar add-on's package ships one `bar { widget "<name>" { … } }` file
//! per premade in [`CATALOG_DIR`]. While `taskbar-widgets` is on, the loader
//! reads them as the lowest config layer, before `/etc/eclipse/abyss.kdl`, and
//! keeps a copy in [`super::Config::catalog`]: a catalog block is approved by
//! virtue of shipping, and its hash is the reference an edited copy is
//! compared against.
//!
//! A catalog file may contain only `bar { widget … }`. Anything else is
//! ignored with a warning, and a widget block that does not validate is
//! skipped with a warning rather than refusing the whole config: a broken
//! package file must not stop the compositor starting.
//!
//! The directory is watched like the add-on directory (`addons.rs`) by
//! `ec-abyss`'s `config::catalog::start`; only the reading lives here.

use std::path::{Path, PathBuf};

use kdl::KdlDocument;

use super::{schema, Config, CustomWidget, Source};

/// Package-owned and root-writable, like `addons::SYSTEM_DIR`. A constant:
/// never an env var, a CLI flag or a config key, so a user-level process
/// cannot plant a "premade". Only tests point the loader elsewhere, through
/// the thread-local override below.
pub const CATALOG_DIR: &str = "/usr/share/eclipse/widgets";

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Tests only: read the catalog from `dir` on this thread (`None` = empty).
/// Behind the `test-hooks` feature (not just `cfg(test)`) because `ec-abyss`'s
/// own tests call it across the crate boundary.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn set_test_dir(dir: Option<PathBuf>) {
    TEST_DIR.with(|d| *d.borrow_mut() = dir);
}

/// The catalog, in sorted file order. A missing directory is an empty catalog.
pub fn load() -> Vec<CustomWidget> {
    #[cfg(any(test, feature = "test-hooks"))]
    {
        match TEST_DIR.with(|d| d.borrow().clone()) {
            Some(dir) => load_from(&dir),
            None => Vec::new(),
        }
    }
    #[cfg(not(any(test, feature = "test-hooks")))]
    {
        load_from(Path::new(CATALOG_DIR))
    }
}

fn load_from(dir: &Path) -> Vec<CustomWidget> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "reading the widget catalog; running with none");
            Vec::new()
        }
    };
    files.sort();
    let mut out: Vec<CustomWidget> = Vec::new();
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "reading a catalog widget; skipped");
                continue;
            }
        };
        for w in parse_file(&path, &text) {
            if out.iter().any(|o| o.name == w.name) {
                tracing::warn!(file = %path.display(), name = w.name, "catalog widget name given twice; the first one wins");
                continue;
            }
            out.push(w);
        }
    }
    out
}

/// The valid `bar { widget … }` blocks of one catalog file, through the same
/// parser a user block goes through.
fn parse_file(path: &Path, text: &str) -> Vec<CustomWidget> {
    let doc: KdlDocument = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(file = %path.display(), error = %e, "catalog file is not KDL; skipped");
            return Vec::new();
        }
    };
    let src = Source {
        path: path.to_path_buf(),
        owner: schema::Owner::Abyss,
    };
    let mut out = Vec::new();
    for node in doc.nodes() {
        if node.name().value() != "bar" {
            tracing::warn!(file = %path.display(), node = node.name().value(), "catalog files hold only bar {{ widget … }}; ignored");
            continue;
        }
        for child in node.children().map(|c| c.nodes()).unwrap_or_default() {
            if child.name().value() != "widget" {
                tracing::warn!(file = %path.display(), node = child.name().value(), "catalog files hold only bar {{ widget … }}; ignored");
                continue;
            }
            let mut scratch = Config {
                cur: Some((src.clone(), text.to_owned())),
                ..Config::default()
            };
            scratch.apply_bar_widget(child);
            if !scratch.errors.is_empty() {
                // `reject` has already logged each refusal with its position.
                tracing::warn!(file = %path.display(), "catalog widget refused by the parser; skipped");
                continue;
            }
            out.extend(scratch.bar.custom_widgets);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CustomWidgetKind;

    fn dir(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("abyss-catalog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (name, text) in files {
            std::fs::write(d.join(name), text).unwrap();
        }
        d
    }

    #[test]
    fn the_shipped_premades_load_as_a_catalog() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packaging/widgets");
        let cat = load_from(&d);
        let names: Vec<&str> = cat.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["failed-units", "load", "uptime"]);
        assert!(cat.iter().all(|w| crate::widget_hash::hash(w).is_some()));
    }

    #[test]
    fn only_widget_blocks_count_and_bad_ones_are_skipped() {
        let d = dir(
            "mixed",
            &[
                (
                    "a.kdl",
                    "general { gaps-in 99; }\nbar {\n    rounding 0\n    widget \"a\" { exec \"true\"; }\n    \
                     widget \"bad\" { exec \"x\"; interval-ms 1; }\n}\nbind \"Super\" \"x\" \"exec\" \"rm\"\n",
                ),
                ("b.kdl", "bar { widget \"a\" { exec \"second\"; }; widget \"b\" { exec \"b\"; stream #true; } }"),
                ("c.kdl", "{{{"),
                ("notes.txt", "bar { widget \"n\" { exec \"n\"; } }"),
            ],
        );
        let cat = load_from(&d);
        let names: Vec<&str> = cat.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(
            cat[0].kind,
            CustomWidgetKind::Exec {
                argv: vec!["true".into()],
                interval_ms: schema::WIDGET_DEFAULT_INTERVAL_MS
            },
            "the first file's `a` wins"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_missing_directory_is_an_empty_catalog() {
        let d = std::env::temp_dir().join(format!("abyss-catalog-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(load_from(&d).is_empty());
    }

    /// ADR 0067: the catalog is package-owned. Nothing but the test override
    /// names another directory.
    #[test]
    fn the_catalog_directory_is_not_user_controlled() {
        assert_eq!(CATALOG_DIR, "/usr/share/eclipse/widgets");
        let src = include_str!("catalog.rs");
        assert!(
            !src.contains(concat!("env", "::var")),
            "catalog.rs reads the environment"
        );
        assert!(
            !src.contains(concat!("pub fn load", "_from")),
            "the directory became a public parameter"
        );
        for key in crate::schema::TABLE {
            assert!(!key.path.contains("catalog"), "{} is a config key", key.path);
        }
    }
}
