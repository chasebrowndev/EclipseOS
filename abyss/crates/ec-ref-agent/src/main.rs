// SPDX-License-Identifier: Apache-2.0
//! `ec-ref-agent`: connect to `$ECLIPSE_MCP_SOCKET` and run the reference
//! behaviour until the task's conversation closes.

use std::{os::unix::net::UnixStream, process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let Some(path) = std::env::var_os("ECLIPSE_MCP_SOCKET") else {
        eprintln!("ec-ref-agent: ECLIPSE_MCP_SOCKET is not set");
        return ExitCode::from(2);
    };
    let sock = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ec-ref-agent: cannot connect to the task socket: {e}");
            return ExitCode::from(1);
        }
    };
    match ec_ref_agent::run(sock, Duration::from_millis(500)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-ref-agent: {e}");
            ExitCode::from(1)
        }
    }
}
