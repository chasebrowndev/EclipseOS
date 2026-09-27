// SPDX-License-Identifier: AGPL-3.0-only
//! The command line: two subcommands and no options. Nothing the caller puts in
//! argv or the environment selects a path, a disk, a package or a command.
//!
//! * `list-disks`: unprivileged. Prints a JSON array of `eclipse_setup_plan::Disk`
//!   from the helper's own listing, the boot medium's disk excluded.
//! * `apply`: one JSON `Request` on stdin (at most [`MAX_REQUEST_BYTES`]), one
//!   `Progress` JSON object per line on stdout.

use crate::apply::apply;
use crate::disks;
use crate::env::Env;
use eclipse_setup_plan::{Progress, Request, Stage};
use std::ffi::OsString;
use std::io::{Read, Write};
use zeroize::Zeroizing;

pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

/// Read and parse the request. Every failure is the same fixed answer: serde's
/// own messages quote the offending value, and the value may be the password.
fn read_request(stdin: &mut dyn Read) -> Result<Request, &'static str> {
    let mut buf = Zeroizing::new(Vec::new());
    stdin
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| "request unreadable")?;
    if buf.len() as u64 > MAX_REQUEST_BYTES {
        return Err("request too large");
    }
    serde_json::from_slice(&buf).map_err(|_| "request not valid")
}

fn fail_line(stdout: &mut dyn Write, msg: &str) {
    let p = Progress {
        stage: Stage::Validate,
        pct: 0,
        msg: msg.to_owned(),
        failed: true,
    };
    if let Ok(mut s) = serde_json::to_string(&p) {
        s.push('\n');
        let _ = stdout.write_all(s.as_bytes());
        let _ = stdout.flush();
    }
}

pub fn run(
    args: &[OsString],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &Env,
) -> i32 {
    match args {
        [c] if c == "list-disks" => match disks::list_offered(&env.paths, env.runner) {
            Ok(list) => match serde_json::to_writer(&mut *stdout, &list) {
                Ok(()) => {
                    let _ = stdout.write_all(b"\n");
                    EXIT_OK
                }
                Err(_) => EXIT_FAILED,
            },
            Err(e) => {
                let _ = writeln!(stderr, "eclipse-setup-helper: {e}");
                EXIT_FAILED
            }
        },
        [c] if c == "apply" => match read_request(stdin) {
            Ok(req) => {
                if apply(env, &req, stdout) {
                    EXIT_OK
                } else {
                    EXIT_FAILED
                }
            }
            Err(why) => {
                fail_line(stdout, why);
                EXIT_FAILED
            }
        },
        _ => {
            let _ = writeln!(stderr, "usage: eclipse-setup-helper list-disks | apply");
            EXIT_USAGE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::DenyConfirm;
    use crate::testutil::{live, machine, req, runner};

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    fn drive(a: &[&str], stdin: &[u8], euid: u32, with_live: bool) -> (i32, String, String) {
        let (_t, p) = if with_live { live() } else { machine() };
        let r = runner();
        let env = Env {
            paths: p,
            euid,
            caller_uid_hint: None,
            runner: &r,
            confirm: &DenyConfirm,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args(a), &mut &stdin[..], &mut out, &mut err, &env);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn body(pw: &str) -> Vec<u8> {
        let mut v = serde_json::to_value(&req().plan).unwrap();
        v = serde_json::json!({"plan": v, "password": pw});
        serde_json::to_vec(&v).unwrap()
    }

    #[test]
    fn exactly_two_subcommands_and_no_options() {
        for a in [
            &[][..],
            &["apply", "--disk", "/dev/sda"],
            &["list-disks", "x"],
            &["--help"],
            &["APPLY"],
            &["sh"],
        ] {
            let (code, out, _) = drive(a, b"", 0, true);
            assert_eq!((code, out.as_str()), (EXIT_USAGE, ""), "{a:?}");
        }
    }

    #[test]
    fn list_disks_prints_the_offered_disks_only() {
        let (code, out, _) = drive(&["list-disks"], b"", 1000, false);
        assert_eq!(code, EXIT_OK);
        let v: Vec<eclipse_setup_plan::Disk> = serde_json::from_str(&out).unwrap();
        let ids: Vec<_> = v.iter().map(|d| d.by_id.as_str()).collect();
        assert_eq!(ids, ["ata-Samsung_SSD_S1", "nvme-Some_NVMe_1"]);
    }

    #[test]
    fn apply_refuses_when_not_root() {
        let (code, out, _) = drive(&["apply"], &body("pw pw"), 1000, true);
        assert_eq!(code, EXIT_FAILED);
        let p: Progress = serde_json::from_str(out.lines().last().unwrap()).unwrap();
        assert!(p.failed && p.stage == Stage::Validate && p.msg == "refused: not root");
    }

    #[test]
    fn unparseable_and_oversized_requests_are_refused_without_echo() {
        for (bad, why) in [
            (b"{\"plan\": TOPSECRET".to_vec(), "request not valid"),
            (
                br#"{"plan":{"disk_by_id":5},"password":"TOPSECRET"}"#.to_vec(),
                "request not valid",
            ),
            (vec![b' '; MAX_REQUEST_BYTES as usize + 1], "request too large"),
        ] {
            let (code, out, err) = drive(&["apply"], &bad, 0, true);
            assert_eq!(code, EXIT_FAILED);
            let p: Progress = serde_json::from_str(out.trim()).unwrap();
            assert_eq!(p.msg, why);
            assert!(!out.contains("TOPSECRET") && !err.contains("TOPSECRET"));
        }
    }

    #[test]
    fn a_password_with_newline_is_refused_before_anything_runs() {
        let (code, out, _) = drive(&["apply"], &body("a\nroot:x"), 0, true);
        assert_eq!(code, EXIT_FAILED);
        assert!(out.contains("refused: password") && !out.contains("root:x"));
    }

    #[test]
    fn default_helper_stops_at_confirm() {
        let (code, out, _) = drive(&["apply"], &body("correct horse"), 0, true);
        assert_eq!(code, EXIT_FAILED);
        let p: Progress = serde_json::from_str(out.lines().last().unwrap()).unwrap();
        assert_eq!((p.stage, p.failed), (Stage::Confirm, true));
        assert!(!out.contains("horse"));
    }
}
