// SPDX-License-Identifier: AGPL-3.0-only
use ec_setup_helper::cli;
use ec_setup_helper::confirm::{Confirm, TypedConfirm};
use ec_setup_helper::env::{Env, Paths};
use ec_setup_helper::runner::SysRunner;
use rustix::fs::Mode;
use rustix::process::{geteuid, setrlimit, Resource, Rlimit};
use std::process::ExitCode;

fn main() -> ExitCode {
    // The password passes through this process's memory: no core dumps, and a
    // predictable mask for what it creates.
    let _ = setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    );
    rustix::process::umask(Mode::from_raw_mode(0o022));

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // The calling user, from pkexec. A hint that says only where the
    // compositor's socket is and who must own it; the peer check is the trust.
    let caller_uid_hint = std::env::var("PKEXEC_UID").ok();
    let confirm: &dyn Confirm = &TypedConfirm;
    let env = Env {
        paths: Paths::system(),
        euid: geteuid().as_raw(),
        caller_uid_hint,
        runner: &SysRunner,
        confirm,
    };
    let code = cli::run(
        &args,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        &env,
    );
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
