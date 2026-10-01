// SPDX-License-Identifier: Apache-2.0
//! P-04 §8 FFI fixture (ADR 0034, COMP-16 milestone 21): the throughput,
//! echo-off and OSC 133 fixtures run through the C ABI, from C, against the
//! staticlib `cataclysm` links, under AddressSanitizer, LeakSanitizer and
//! UBSan. This file only builds and runs `tests/c/fixtures.c`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The staticlib cargo built alongside the rlib this test links. Integration
/// test binaries live in the same `deps/` directory.
fn staticlib() -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let deps = exe.parent().expect("deps dir");
    let newest = std::fs::read_dir(deps)
        .expect("read deps")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libec_cataclysm_pub-") && n.ends_with(".a"))
        })
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    newest.expect("libec_cataclysm_pub-*.a next to the test binary (crate-type staticlib)")
}

#[test]
fn p04_fixtures_through_the_c_abi_under_asan() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ecpub_fixtures");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let status = Command::new(&cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O1", "-g"])
        .args([
            "-fsanitize=address,undefined",
            "-fno-sanitize-recover=all",
            "-fno-omit-frame-pointer",
        ])
        .arg("-I")
        .arg(crate_dir.join("include"))
        .arg(crate_dir.join("tests/c/fixtures.c"))
        .arg(staticlib())
        .args(["-lpthread", "-ldl", "-lm", "-lgcc_s", "-lc", "-o"])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("running {cc}: {e}"));
    assert!(status.success(), "compiling the C fixtures failed");

    let run = Command::new(&out)
        .env("ASAN_OPTIONS", "detect_leaks=1:abort_on_error=0:halt_on_error=1")
        .env("UBSAN_OPTIONS", "halt_on_error=1:print_stacktrace=1")
        .output()
        .expect("run fixtures");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    print!("{stdout}");
    eprint!("{stderr}");
    assert!(run.status.success(), "C fixtures failed: {}", run.status);
    assert!(stdout.contains("all fixtures passed"));
}
