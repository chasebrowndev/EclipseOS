// SPDX-License-Identifier: AGPL-3.0-only
//! Stable replay of the fuzz seed corpora (T-18 §11, T-25 §4). `apotheosis/fuzz/` is a
//! separate nightly cargo-fuzz workspace that the workspace gate never builds; this test
//! runs every target's body over every seed in `fuzz/corpus/<target>/` on the pinned
//! stable toolchain, and holds the wiring together: each target has a wrapper in
//! `fuzz/fuzz_targets/`, a `[[bin]]` in `fuzz/Cargo.toml`, a non-empty corpus, a body
//! here, and a wrapper that calls that body.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Target name, the body path its wrapper calls, and the body.
type Target = (&'static str, &'static str, fn(&[u8]));

const TARGETS: &[Target] = &[
    ("vt_parse", "apoth_vt::fuzz_parse", apoth_vt::fuzz_parse),
    (
        "term_feed",
        "apoth_term::fuzz::fuzz_term_feed",
        apoth_term::fuzz::fuzz_term_feed,
    ),
    (
        "osc_all",
        "apoth_term::fuzz::fuzz_osc",
        apoth_term::fuzz::fuzz_osc,
    ),
    (
        "kitty_keyboard_decode",
        "apoth_term::fuzz::fuzz_kitty_keyboard",
        apoth_term::fuzz::fuzz_kitty_keyboard,
    ),
    (
        "kitty_apc",
        "apoth_term::fuzz::fuzz_kitty_apc",
        apoth_term::fuzz::fuzz_kitty_apc,
    ),
    (
        "media_apc",
        "apoth_term::fuzz::fuzz_media_apc",
        apoth_term::fuzz::fuzz_media_apc,
    ),
    (
        "sixel",
        "apoth_term::fuzz::fuzz_sixel",
        apoth_term::fuzz::fuzz_sixel,
    ),
    (
        "placeholder",
        "apoth_term::fuzz::fuzz_placeholder",
        apoth_term::fuzz::fuzz_placeholder,
    ),
    (
        "iterm",
        "apoth_term::fuzz::fuzz_iterm",
        apoth_term::fuzz::fuzz_iterm,
    ),
    (
        "image_formats",
        "apoth_term::fuzz::fuzz_image_formats",
        apoth_term::fuzz::fuzz_image_formats,
    ),
    (
        "config_parse",
        "apoth_config::fuzz::fuzz_config",
        apoth_config::fuzz::fuzz_config,
    ),
    (
        "match_lang",
        "apoth_config::fuzz::fuzz_match",
        apoth_config::fuzz::fuzz_match,
    ),
    (
        "template",
        "apoth_config::fuzz::fuzz_template",
        apoth_config::fuzz::fuzz_template,
    ),
    (
        "proto_json",
        "apoth_proto::fuzz_parse",
        apoth_proto::fuzz_parse,
    ),
    (
        "mux_wire",
        "apoth_proto::mux::fuzz_mux",
        apoth_proto::mux::fuzz_mux,
    ),
    (
        "helper_wire",
        "apoth_media::fuzz::fuzz_helper_wire",
        apoth_media::fuzz::fuzz_helper_wire,
    ),
    (
        "mkv_demux",
        "apoth_media::fuzz::fuzz_mkv_demux",
        apoth_media::fuzz::fuzz_mkv_demux,
    ),
    (
        "av1_obu",
        "apoth_media::fuzz::fuzz_av1_obu",
        apoth_media::fuzz::fuzz_av1_obu,
    ),
];

fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz")
}

fn names_in(dir: &Path, strip: &str) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter_map(|n| n.strip_suffix(strip).map(str::to_owned))
        .collect()
}

#[test]
fn every_seed_holds_its_targets_invariants() {
    for (name, _, body) in TARGETS {
        let dir = fuzz_dir().join("corpus").join(name);
        let mut seeds = 0;
        for e in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = e.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            // A panic here names the seed (file names are not content).
            let r = std::panic::catch_unwind(|| body(&bytes));
            assert!(
                r.is_ok(),
                "{name}: seed {} broke an invariant",
                path.display()
            );
            seeds += 1;
        }
        assert!(seeds > 0, "{name}: empty corpus");
    }
}

#[test]
fn targets_wrappers_bins_and_corpora_agree() {
    let table: BTreeSet<String> = TARGETS.iter().map(|t| t.0.to_owned()).collect();
    assert_eq!(table.len(), TARGETS.len(), "a target is listed twice");
    let wrappers = names_in(&fuzz_dir().join("fuzz_targets"), ".rs");
    let corpora = names_in(&fuzz_dir().join("corpus"), "");
    assert_eq!(wrappers, table, "fuzz_targets/ vs the replay table");
    assert_eq!(corpora, table, "corpus/ vs the replay table");

    let manifest = std::fs::read_to_string(fuzz_dir().join("Cargo.toml")).unwrap();
    for name in &table {
        let bin = format!("[[bin]]\nname = \"{name}\"\npath = \"fuzz_targets/{name}.rs\"\n");
        assert!(
            manifest.contains(&bin),
            "{name}: no [[bin]] in fuzz/Cargo.toml"
        );
    }
    assert_eq!(manifest.matches("\n[[bin]]\n").count(), table.len());
    assert!(
        manifest.contains("\n[workspace]\n"),
        "fuzz/ must be its own workspace"
    );
}

#[test]
fn each_wrapper_calls_its_replayed_body() {
    for (name, path, _) in TARGETS {
        let src =
            std::fs::read_to_string(fuzz_dir().join(format!("fuzz_targets/{name}.rs"))).unwrap();
        assert!(
            src.starts_with("// SPDX-License-Identifier: "),
            "{name}: SPDX"
        );
        assert!(
            src.contains("#![no_main]") && src.contains("fuzz_target!"),
            "{name}"
        );
        assert!(
            src.contains(&format!("{path}(data)")),
            "{name}: wrapper does not call {path}"
        );
    }
}

#[test]
fn the_fuzz_workspace_pins_a_dated_nightly() {
    let tc = std::fs::read_to_string(fuzz_dir().join("rust-toolchain.toml")).unwrap();
    let line = tc
        .lines()
        .find(|l| l.starts_with("channel"))
        .expect("channel");
    let date = line
        .split('"')
        .nth(1)
        .and_then(|c| c.strip_prefix("nightly-"))
        .expect("nightly");
    assert_eq!(date.len(), 10, "dated nightly, not {line}");
}
