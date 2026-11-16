// SPDX-License-Identifier: AGPL-3.0-only
//! The sandboxed media helper (T-16 §4). Started by the terminal only; see
//! `apoth_media::helper`.
#![forbid(unsafe_code)]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(apoth_media::helper::main(&args));
}
