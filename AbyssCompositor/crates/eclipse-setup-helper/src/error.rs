// SPDX-License-Identifier: AGPL-3.0-only
//! Errors are fixed vocabulary on purpose. They end up on a `Progress` line and
//! in front of the user, so none may carry file content, a path the caller
//! influenced, a command's stderr or anything derived from the password
//! (D-07 §5 "never echoing file content", root invariant 6).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The request or the machine failed a guard. Nothing was changed.
    Refused(&'static str),
    /// A filesystem or process operation failed; the context is a fixed string.
    Io(&'static str),
    /// A fixed-argv command exited non-zero or could not be spawned.
    Command { tool: &'static str, code: Option<i32> },
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(why) => write!(f, "refused: {why}"),
            Error::Io(ctx) => write!(f, "i/o failure: {ctx}"),
            Error::Command { tool, code: Some(c) } => write!(f, "command failed: {tool} (exit {c})"),
            Error::Command { tool, code: None } => write!(f, "command failed: {tool}"),
        }
    }
}

impl std::error::Error for Error {}

/// `map_err(io("what"))`: discards the source error, keeps only the context.
pub fn io<E>(ctx: &'static str) -> impl FnOnce(E) -> Error {
    move |_| Error::Io(ctx)
}
