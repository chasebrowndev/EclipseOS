// SPDX-License-Identifier: Apache-2.0
//! `gen-tables [--check] [UCD_DIR]`: write (or verify) `src/tables.rs`.
//! UCD_DIR defaults to `data/ucd` relative to the crate dir (run from `crates/apoth-unicode`, or pass the path).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut check = false;
    let mut dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/ucd"));
    for a in std::env::args().skip(1) {
        if a == "--check" {
            check = true;
        } else {
            dir = PathBuf::from(a);
        }
    }
    let target = PathBuf::from("crates/apoth-unicode/src/tables.rs");
    let text = match apoth_unicode::generator::generate(&dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("gen-tables: {e}");
            return ExitCode::FAILURE;
        }
    };
    if check {
        return match std::fs::read_to_string(&target) {
            Ok(old) if old == text => ExitCode::SUCCESS,
            _ => {
                eprintln!("gen-tables: {} is stale", target.display());
                ExitCode::FAILURE
            }
        };
    }
    match std::fs::write(&target, text) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gen-tables: {e}");
            ExitCode::FAILURE
        }
    }
}
