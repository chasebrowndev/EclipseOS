// SPDX-License-Identifier: AGPL-3.0-only
//! The session's one BlueZ pairing agent.
//!
//! BlueZ takes one agent per session, not one per monitor, so it lives on a
//! thread of its own beside the bars rather than in any of them. It never
//! touches the UI: every question it is asked is answered by an
//! `eclipse-secret-prompt` window it starts, and that window answers the agent
//! itself.

use eclipse_services::status::{Actions, BtPrompt, Event, Events, PairingAgent};
use std::time::{Duration, Instant};

/// How long a code-display window stays up. BlueZ sends no word when the
/// device finishes typing the code, and the pair call's outcome goes to the
/// bar that asked, not here; the kernel gives up on passkey entry after 30 s.
const SHOW_FOR: Duration = Duration::from_secs(30);

/// How often the agent looks for a question and for a finished prompt.
const POLL: Duration = Duration::from_millis(500);

/// Register the agent and answer it for as long as the process lives.
pub fn spawn() {
    let spawned = std::thread::Builder::new().name("pairing".into()).spawn(|| {
        let (actions, events) = match eclipse_services::status::actions(PairingAgent::Register) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("hyperion: no bluetooth pairing agent: {e}");
                return;
            }
        };
        let mut prompt = None;
        loop {
            pair(&actions, &events, &mut prompt);
            std::thread::sleep(POLL);
        }
    });
    if let Err(e) = spawned {
        eprintln!("hyperion: no bluetooth pairing agent: {e}");
    }
}

/// The one pairing window, and the arguments it was started with. No
/// `Debug`: the arguments carry the pairing code.
struct Prompt {
    child: std::process::Child,
    args: Vec<String>,
    /// When a code-display window closes on its own. `None` for one that
    /// waits on an answer: BlueZ cancels those itself.
    until: Option<Instant>,
}

/// Answer what the pairing agent asks. A PIN or passkey is typed into
/// `eclipse-secret-prompt`, which answers the agent itself; this process never
/// holds one. A yes/no is answered by the same window; a code to type on the
/// device is only shown by it.
fn pair(actions: &Actions, events: &Events, prompt: &mut Option<Prompt>) {
    while let Some(event) = events.try_recv() {
        match event {
            Event::BtNeedsSecret { addr, kind } => {
                // What the prompt shows, and whether it has an answer to give.
                // A displayed code opened no request on the agent, so there
                // is nothing to refuse if its window cannot be started.
                // The code rides on argv: a pairing number is not a secret,
                // and it is never printed here.
                let (mode, answers): (Vec<String>, bool) = match kind {
                    BtPrompt::Pin => (vec!["pin".into()], true),
                    BtPrompt::Passkey => (vec!["passkey".into()], true),
                    BtPrompt::Confirm(n) => (vec!["confirm".into(), n.to_string()], true),
                    BtPrompt::Authorize => (vec!["authorize".into()], true),
                    BtPrompt::DisplayPin(pin) => (vec!["show".into(), pin], false),
                    BtPrompt::DisplayPasskey(n) => (vec!["show".into(), format!("{n:06}")], false),
                };
                let args: Vec<String> = ["bt".to_owned(), addr.clone()].into_iter().chain(mode).collect();
                // BlueZ repeats DisplayPasskey on every key the device reports
                // typed; the window already showing that code stays up rather
                // than flickering through a respawn per keystroke.
                if prompt.as_ref().is_some_and(|p| p.args == args) {
                    continue;
                }
                close(prompt);
                match tied(std::process::Command::new("eclipse-secret-prompt").args(&args)).spawn() {
                    Ok(child) => {
                        let until = (!answers).then(|| Instant::now() + SHOW_FOR);
                        *prompt = Some(Prompt { child, args, until });
                    }
                    Err(e) => {
                        eprintln!("hyperion: cannot start the pairing prompt: {e}");
                        if answers {
                            actions.bt_answer(addr, None);
                        }
                    }
                }
            }
            // The device gave up; so does its prompt.
            Event::BtPromptCancelled => close(prompt),
            _ => {}
        }
    }
    if prompt
        .as_ref()
        .and_then(|p| p.until)
        .is_some_and(|t| Instant::now() >= t)
    {
        close(prompt);
    }
    // Reap a prompt that finished on its own.
    if prompt
        .as_mut()
        .is_some_and(|p| !matches!(p.child.try_wait(), Ok(None)))
    {
        *prompt = None;
    }
}

fn close(prompt: &mut Option<Prompt>) {
    if let Some(mut p) = prompt.take() {
        let _ = p.child.kill();
        let _ = p.child.wait();
    }
}

/// Have `cmd`'s process receive SIGTERM when this one dies, however it dies,
/// so a pairing window never outlives the bar that answers for it.
///
/// `PR_SET_PDEATHSIG` fires on the death of the *thread* that forked, not the
/// process; the pairing thread lives as long as the process, so the two are
/// the same here.
fn tied(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: `prctl` and `getppid` are async-signal-safe and touch no memory
    // of the forked copy; nothing here allocates.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent died between fork and prctl: the signal will never
            // come, so go now.
            if libc::getppid() != parent {
                libc::_exit(0);
            }
            Ok(())
        })
    };
    cmd
}
