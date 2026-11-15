// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth askpass PROMPT` (T-22 §3): what `SSH_ASKPASS` runs for an `apoth ssh` session.
//! It connects to the armed socket named by `APOTHEOSIS_ASKPASS_SOCK`, sends the token and
//! the prompt, and prints the answer for ssh on stdout. Without an armed channel, with a bad
//! token or when the instance refuses, it exits 3 and nothing is shown.
//!
//! The secret lives in one stack buffer, is written once to stdout (the pipe ssh reads) and
//! wiped; it is never logged or stored. Exit: 0 answered, 1 cancelled, 3 refused.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use apoth_proto::askpass::{
    ENV_SOCK, ENV_TOKEN, Kind, MAX_SECRET, Status, TOKEN_HEX, encode_request, wipe,
};

/// Refused (T-12 §5 "denied").
pub(crate) const REFUSED: i32 = 3;
/// How long the user may take to answer the sheet.
const ANSWER_WITHIN: Duration = Duration::from_secs(600);

pub(crate) fn run(args: &[String]) -> i32 {
    let prompt = args.first().map_or("", String::as_str);
    let (Some(sock), Some(token)) = (std::env::var_os(ENV_SOCK), std::env::var_os(ENV_TOKEN))
    else {
        eprintln!("apoth askpass: no armed askpass channel (run ssh through `apoth ssh`)");
        return REFUSED;
    };
    let token = token.as_encoded_bytes();
    if token.len() != TOKEN_HEX {
        eprintln!("apoth askpass: refused");
        return REFUSED;
    }
    let kind = Kind::from_env(std::env::var("SSH_ASKPASS_PROMPT").ok().as_deref());
    let Ok(mut s) = UnixStream::connect(&sock) else {
        eprintln!("apoth askpass: refused");
        return REFUSED;
    };
    let _ = s.set_read_timeout(Some(ANSWER_WITHIN));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    if s.write_all(&encode_request(kind, token, prompt.as_bytes()))
        .and_then(|()| s.shutdown(Shutdown::Write))
        .is_err()
    {
        eprintln!("apoth askpass: refused");
        return REFUSED;
    }
    // Status byte + secret + one byte to notice an over-long answer.
    let mut buf = [0u8; 1 + MAX_SECRET + 1];
    let mut n = 0;
    while n < buf.len() {
        match s.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let code = match buf
        .first()
        .copied()
        .filter(|_| n > 0)
        .and_then(Status::from_byte)
    {
        Some(Status::Ok) if n <= 1 + MAX_SECRET => {
            let mut out = std::io::stdout().lock();
            let ok = out
                .write_all(&buf[1..n])
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush());
            if ok.is_ok() { 0 } else { 1 }
        }
        Some(Status::Cancelled) => 1,
        _ => {
            eprintln!("apoth askpass: refused");
            REFUSED
        }
    };
    wipe(&mut buf);
    code
}
