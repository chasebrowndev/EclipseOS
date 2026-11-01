// SPDX-License-Identifier: AGPL-3.0-only
//! Firefox native-messaging host for the glass extension (ADR 0079).
//!
//! stdout carries the protocol and nothing else; diagnostics go to stderr and
//! never include message contents.
//!
//! `ec-glass-host --remove <profile-dir>` instead takes the managed CSS out of
//! a profile and exits.

use std::io;
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--remove") {
        let Some(dir) = args.next() else {
            eprintln!("usage: ec-glass-host --remove <profile-dir>");
            std::process::exit(2);
        };
        if let Err(e) = ec_glass_host::profile::uninstall(Path::new(&dir)) {
            eprintln!("ec-glass-host: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Best effort: the relay works without the CSS.
    match ec_glass_host::profile::find_profile(std::os::unix::process::parent_id()) {
        Some(p) => {
            if let Err(e) = ec_glass_host::profile::install(&p) {
                eprintln!("ec-glass-host: profile css: {e}");
            }
        }
        None => eprintln!("ec-glass-host: no Firefox profile found; skipping css"),
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut host = ec_glass_host::Host::new(ec_glass_host::Abyss::default());
    if let Err(e) = host.run(&mut stdin.lock(), &mut stdout.lock()) {
        eprintln!("ec-glass-host: {e}");
        std::process::exit(1);
    }
}
