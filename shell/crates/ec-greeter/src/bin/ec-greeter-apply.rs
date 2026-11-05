// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-greeter-apply`: the root entry point of the login screen settings (ADR
//! 0088). **TCB.** Installed to /usr/lib/eclipse/ec-greeter-apply and run only
//! through pkexec under polkit action org.eclipse.greeter.apply.
//!
//!     ec-greeter-apply apply   < settings and raw RGBA, see `ec_greeter::Input`
//!     ec-greeter-apply reset
//!
//! Any other argv exits 2 before anything else. Widening this is the privilege
//! change to review. Never parses an image from a user: it encodes raw pixels.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use ec_greeter::{apply, reset, Error, Input, MAX_INPUT, STATE_DIR};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [verb] = args.as_slice() else {
        return ExitCode::from(2);
    };
    let verb = verb.to_str().unwrap_or("");
    if verb != "apply" && verb != "reset" {
        return ExitCode::from(2);
    }
    // SAFETY: `geteuid` is always safe; it cannot fail and touches no memory.
    if unsafe { libc::geteuid() } != 0 {
        return ExitCode::from(1);
    }
    let dir = Path::new(STATE_DIR);
    let result = if verb == "reset" {
        reset(dir, 0)
    } else {
        read_input().and_then(|input| apply(dir, 0, &input))
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-greeter-apply: {e}");
            ExitCode::from(if matches!(e, Error::Invalid(_)) { 2 } else { 3 })
        }
    }
}

fn read_input() -> Result<Input, Error> {
    let mut bytes = Vec::new();
    // One byte over, so an oversize input fails `Input::parse` on its length.
    std::io::stdin()
        .lock()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Io(format!("stdin: {e}")))?;
    Input::parse(bytes)
}
