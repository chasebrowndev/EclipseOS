// SPDX-License-Identifier: AGPL-3.0-only
//! Firefox native-messaging host for the glass extension (ADR 0079).
//!
//! stdout carries the protocol and nothing else; diagnostics go to stderr and
//! never include message contents.

use std::io;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut host = ec_glass_host::Host::new(ec_glass_host::Abyss::default());
    if let Err(e) = host.run(&mut stdin.lock(), &mut stdout.lock()) {
        eprintln!("ec-glass-host: {e}");
        std::process::exit(1);
    }
}
