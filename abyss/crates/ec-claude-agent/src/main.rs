// SPDX-License-Identifier: Apache-2.0
//! `ec-claude-agent`: connect to `$ECLIPSE_MCP_SOCKET` and converse through
//! the task's `inference.complete` tool until the conversation closes.

use std::{os::unix::net::UnixStream, process::ExitCode};

fn main() -> ExitCode {
    let Some(path) = std::env::var_os("ECLIPSE_MCP_SOCKET") else {
        eprintln!("ec-claude-agent: ECLIPSE_MCP_SOCKET is not set");
        return ExitCode::from(2);
    };
    let sock = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ec-claude-agent: cannot connect to the task socket: {e}");
            return ExitCode::from(1);
        }
    };
    match ec_claude_agent::run(sock, ec_claude_agent::Backoff::default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-claude-agent: {e}");
            ExitCode::from(1)
        }
    }
}
