// SPDX-License-Identifier: AGPL-3.0-only
//! Reading a value or passphrase without echo, into locked memory.
//!
//! Echo and signal generation are switched off for the read, so Ctrl-C
//! arrives as a byte (0x03) instead of a SIGINT that would kill the process
//! with the terminal still silent. The guard restores the terminal on every
//! return path, including that one. (SIGTERM/SIGKILL are not caught; run
//! `stty sane` after one.)

use ec_brokerd::hygiene::LockedBuf;
use rustix::io::{read, Errno};
use rustix::stdio::stdin;
use rustix::termios::{isatty, tcgetattr, tcsetattr, LocalModes, OptionalActions, Termios};

/// Longest line accepted; well under brokerd's 32 KiB packet.
pub const MAX_LINE: usize = 8192;

#[derive(Debug, PartialEq, Eq)]
pub enum InputError {
    /// Not a terminal and `--stdin` was not given.
    NotTerminal,
    /// `--stdin` was given but stdin is a terminal.
    IsTerminal,
    Aborted,
    Empty,
    TooLong,
    NoLockedMemory,
    Io,
}

impl InputError {
    pub fn message(&self) -> &'static str {
        match self {
            InputError::NotTerminal => {
                "stdin is not a terminal; run it in a terminal, or pass --stdin to pipe one line"
            }
            InputError::IsTerminal => {
                "--stdin needs piped input; without it the value is read from the terminal"
            }
            InputError::Aborted => "aborted",
            InputError::Empty => "empty input refused",
            InputError::TooLong => "input too long",
            InputError::NoLockedMemory => "cannot lock memory for the value (RLIMIT_MEMLOCK?)",
            InputError::Io => "cannot read input",
        }
    }
}

/// A line held in locked, zero-on-drop memory.
pub struct Line {
    buf: LockedBuf,
    len: usize,
}

impl Line {
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf.as_slice()[..self.len]
    }
}

/// Restores the saved terminal attributes on drop.
struct EchoOff(Termios);

impl EchoOff {
    fn new() -> Result<EchoOff, InputError> {
        let saved = tcgetattr(stdin()).map_err(|_| InputError::Io)?;
        let mut t = saved.clone();
        t.local_modes.remove(LocalModes::ECHO | LocalModes::ISIG);
        tcsetattr(stdin(), OptionalActions::Drain, &t).map_err(|_| InputError::Io)?;
        Ok(EchoOff(saved))
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        let _ = tcsetattr(stdin(), OptionalActions::Drain, &self.0);
    }
}

/// One line from stdin. `piped` selects `--stdin` mode (stdin must not be a
/// terminal); otherwise it must be one, and `prompt` is shown on stderr.
pub fn read_line(prompt: &str, piped: bool) -> Result<Line, InputError> {
    let tty = isatty(stdin());
    if piped && tty {
        return Err(InputError::IsTerminal);
    }
    if !piped && !tty {
        return Err(InputError::NotTerminal);
    }
    let mut line = Line {
        buf: LockedBuf::new(MAX_LINE).map_err(|_| InputError::NoLockedMemory)?,
        len: 0,
    };
    let _guard = if tty {
        eprint!("{prompt}");
        Some(EchoOff::new()?)
    } else {
        None
    };
    let res = fill(&mut line, tty);
    if tty {
        // The newline the user typed was not echoed.
        eprintln!();
    }
    res?;
    Ok(line)
}

fn fill(line: &mut Line, tty: bool) -> Result<(), InputError> {
    let mut b = [0u8; 1];
    let out = loop {
        match read(stdin(), &mut b) {
            Ok(0) => {
                break if line.len == 0 && tty {
                    Err(InputError::Aborted)
                } else {
                    Ok(())
                }
            }
            Ok(_) => match b[0] {
                b'\n' => break Ok(()),
                3 if tty => break Err(InputError::Aborted),
                c => {
                    if line.len == MAX_LINE {
                        break Err(InputError::TooLong);
                    }
                    let n = line.len;
                    line.buf.as_mut_slice()[n] = c;
                    line.len += 1;
                }
            },
            Err(Errno::INTR) => {}
            Err(_) => break Err(InputError::Io),
        }
    };
    zeroize::Zeroize::zeroize(&mut b);
    out?;
    if line.len > 0 && line.as_bytes()[line.len - 1] == b'\r' {
        line.len -= 1;
    }
    if line.len == 0 {
        return Err(InputError::Empty);
    }
    Ok(())
}
