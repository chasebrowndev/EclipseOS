// SPDX-License-Identifier: AGPL-3.0-only
//! The command line: `--help` and `--version`, and nothing else.
//!
//! The locker trusts nothing it is handed. Who to authenticate is the process
//! uid ([`crate::user`]), the PAM service is fixed ([`crate::PAM_SERVICE`]),
//! and there is no option that could make it skip, shorten or fake a check.
//! An unknown argument is refused before anything is locked.

/// What the arguments asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// No arguments: lock.
    Lock,
    Help,
    Version,
}

/// An argument that is not `--help`, `-h` or `--version`, or more than one.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused;

pub const USAGE: &str = "usage: ec-lock [--help | --version]\n\
\n\
Locks the session (ext-session-lock-v1) and unlocks it once the password of\n\
the user running it passes PAM service `ec-lock`. Takes no options.\n";

/// Parse everything after argv[0].
pub fn parse<I, S>(args: I) -> Result<Cmd, Refused>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut it = args.into_iter();
    let Some(first) = it.next() else {
        return Ok(Cmd::Lock);
    };
    if it.next().is_some() {
        return Err(Refused);
    }
    match first.as_ref().to_str() {
        Some("--help" | "-h") => Ok(Cmd::Help),
        Some("--version") => Ok(Cmd::Version),
        _ => Err(Refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_locks() {
        assert_eq!(parse(Vec::<&str>::new()), Ok(Cmd::Lock));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(["--help"]), Ok(Cmd::Help));
        assert_eq!(parse(["-h"]), Ok(Cmd::Help));
        assert_eq!(parse(["--version"]), Ok(Cmd::Version));
    }

    #[test]
    fn anything_else_is_refused() {
        for a in ["--user", "root", "-v", "", "--help=1", "--no-auth", "-"] {
            assert_eq!(parse([a]), Err(Refused), "{a:?}");
        }
        assert_eq!(parse(["--help", "--version"]), Err(Refused));
        assert_eq!(parse(["--version", "x"]), Err(Refused));
    }

    #[test]
    fn non_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"--\xffhelp");
        assert_eq!(parse([raw]), Err(Refused));
    }
}
