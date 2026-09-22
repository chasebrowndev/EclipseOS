// SPDX-License-Identifier: AGPL-3.0-only
//! `hyperion` — the taskbar, a layer surface anchored to the top edge.
//!
//! One of the DE's handful of layer-shell clients (the toast stack and the OSD
//! are the others).
//! Trusted UI is compositor-drawn and never comes through here.
//!
//! Two modes, chosen by the command line:
//!
//! * `--output <NAME>` — one bar, bound to that connector.
//! * no arguments — the **supervisor**: it owns no surface of its own, it
//!   spawns one `--output` child per output and reconciles that set as
//!   monitors come and go.
//!
//! The split exists because a layer-shell surface's output identity never
//! reaches the iced application: `layershellev`'s window wrapper carries only
//! a window id, so a single process bound to every screen at once could not
//! tell which monitor it was drawing on — and a per-output bar's whole job is
//! to show its own monitor's workspaces. One process per output is the only
//! shape where that question has an answer.

use hyperion::{app, view, OUTPUT_ENV};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};

fn namespace() -> String {
    "hyperion".to_owned()
}

fn main() -> iced_layershell::Result {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--output") => match args.next() {
            Some(name) => bar(name),
            None => {
                eprintln!("hyperion: --output needs a connector name");
                std::process::exit(2);
            }
        },
        None => {
            supervise();
            Ok(())
        }
        Some(other) => {
            eprintln!("hyperion: unknown argument {other:?} (expected --output <NAME>)");
            std::process::exit(2);
        }
    }
}

/// One bar on one output.
fn bar(output: String) -> iced_layershell::Result {
    // The bar never reads or writes a selection. Human input is never logged
    // by content, and the simplest way to keep that true is not to hold a
    // clipboard at all.
    iced_layershell::disable_clipboard();
    // Read back by `App::new`, which the builder calls with no arguments.
    std::env::set_var(OUTPUT_ENV, &output);

    // `bar.position` is `reload: restart` — the layer surface's anchor is
    // fixed for its whole life, so it is read once here, before the surface
    // exists, rather than through the live `Conn` the running app holds.
    let position = hyperion::conn::Conn::new().bar_config().position;
    let edge = match position {
        hyperion::conn::BarPosition::Top => Anchor::Top,
        hyperion::conn::BarPosition::Bottom => Anchor::Bottom,
    };
    // The same geometry every fold step asks for, so the first frame and a
    // settled unfold cannot disagree.
    let geometry = app::FoldState::default().geometry(position);

    let mut builder =
        iced_layershell::build_pattern::daemon(app::App::new, namespace, app::update, view::view)
            .layer_settings(LayerShellSettings {
                anchor: edge | Anchor::Left | Anchor::Right,
                layer: Layer::Top,
                // Width 0 means "as wide as the output, less the side
                // margins". The pill fills the surface and the float gap is
                // margin (see `FoldState::geometry`); the zone plus the edge
                // margin is the full strip, so a window opened afterwards
                // starts below the bar rather than under it.
                size: Some((0, geometry.height)),
                margin: geometry.margin,
                exclusive_zone: geometry.zone,
                // The bar is pointer-driven. It must never take the keyboard
                // away from the window the human is typing into.
                keyboard_interactivity: KeyboardInteractivity::None,
                // Without this the request carries a null output and the
                // compositor resolves it to whichever monitor happens to be
                // focused — which is how there came to be exactly one bar.
                start_mode: StartMode::TargetScreen(output),
                ..Default::default()
            })
            .style(view::style)
            .theme(|_: &app::App, _: iced::window::Id| eclipse_ui::theme::theme())
            .subscription(app::subscription)
            .antialiasing(true);
    // Every face, registered once, before the surface exists.
    for face in eclipse_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}

/// Keep one child per output alive for as long as this process lives.
///
/// Every child is tied to this process by [`tied`]: stopping the unit stops
/// every bar, and a vanished monitor's child is killed and reaped here rather
/// than outliving its output.
fn supervise() {
    use std::collections::HashMap;

    let mut conn = hyperion::conn::Conn::new();
    let mut children: HashMap<String, std::process::Child> = HashMap::new();
    let exe = std::env::current_exe().unwrap_or_else(|_| "hyperion".into());
    // The supervisor holds the subscription itself: an output event is the
    // one thing that means the set of monitors may have moved.
    let mut client = None;
    // BlueZ takes one pairing agent per session, not one per monitor, so it
    // lives here and not in the bars.
    let pairing = match eclipse_services::status::actions(eclipse_services::status::PairingAgent::Register) {
        Ok(pair) => Some(pair),
        Err(e) => {
            eprintln!("hyperion: no bluetooth pairing agent: {e}");
            None
        }
    };
    let mut prompt: Option<Prompt> = None;

    loop {
        if let Some((actions, events)) = pairing.as_ref() {
            pair(actions, events, &mut prompt);
        }
        let (outputs, _) = conn.outputs();
        // Only reconcile against an answer. An empty list because the socket
        // is not up yet must not be read as "every monitor went away".
        if !outputs.is_empty() {
            for (_, name) in &outputs {
                if let Some(child) = children.get_mut(name) {
                    // Still running is the common case; a child that exited on
                    // its own (the compositor restarted, say) is replaced.
                    if matches!(child.try_wait(), Ok(None)) {
                        continue;
                    }
                }
                match tied(std::process::Command::new(&exe).arg("--output").arg(name)).spawn() {
                    Ok(child) => {
                        children.insert(name.clone(), child);
                    }
                    Err(e) => eprintln!("hyperion: cannot start a bar on {name}: {e}"),
                }
            }
            children.retain(|name, child| {
                if outputs.iter().any(|(_, live)| live == name) {
                    return true;
                }
                // The monitor is gone. Its surface died with it; kill and reap
                // so the child does not linger as a zombie.
                let _ = child.kill();
                let _ = child.wait();
                false
            });
        }

        if client.is_none() {
            if let Ok(mut c) = eclipse_ipc::Client::connect() {
                if c.subscribe(&[eclipse_ipc::EventKind::Output]).is_ok() {
                    client = Some(c);
                }
            }
        }
        if let Some(c) = client.as_mut() {
            loop {
                match c.poll_event() {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => {
                        client = None;
                        break;
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Have `cmd`'s process receive SIGTERM when this one dies, however it dies.
/// A child shares no session with a unit stop's SIGTERM on its own: without
/// this, every bar outlived a supervisor that was told to stop.
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

/// How long a code-display window stays up. BlueZ sends no word when the
/// device finishes typing the code, and the pair call's outcome goes to the
/// bar that asked, not here; the kernel gives up on passkey entry after 30 s.
const SHOW_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// The one pairing window, and the arguments it was started with. No
/// `Debug`: the arguments carry the pairing code.
struct Prompt {
    child: std::process::Child,
    args: Vec<String>,
    /// When a code-display window closes on its own. `None` for one that
    /// waits on an answer: BlueZ cancels those itself.
    until: Option<std::time::Instant>,
}

/// Answer what the pairing agent asks. A PIN or passkey is typed into
/// `eclipse-secret-prompt`, which answers the agent itself; this process never
/// holds one. A yes/no is answered by the same window; a code to type on the
/// device is only shown by it.
fn pair(
    actions: &eclipse_services::status::Actions,
    events: &eclipse_services::status::Events,
    prompt: &mut Option<Prompt>,
) {
    use eclipse_services::status::{BtPrompt, Event};

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
                        let until = (!answers).then(|| std::time::Instant::now() + SHOW_FOR);
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
        .is_some_and(|t| std::time::Instant::now() >= t)
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
