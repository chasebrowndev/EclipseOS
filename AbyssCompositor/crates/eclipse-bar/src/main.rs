// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-bar` — a layer surface anchored to the top edge.
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

use eclipse_bar::{app, view, HEIGHT, OUTPUT_ENV};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};

fn namespace() -> String {
    "eclipse-bar".to_owned()
}

fn main() -> iced_layershell::Result {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--output") => match args.next() {
            Some(name) => bar(name),
            None => {
                eprintln!("eclipse-bar: --output needs a connector name");
                std::process::exit(2);
            }
        },
        None => {
            supervise();
            Ok(())
        }
        Some(other) => {
            eprintln!("eclipse-bar: unknown argument {other:?} (expected --output <NAME>)");
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
    let edge = match eclipse_bar::conn::Conn::new().bar_config().position {
        eclipse_bar::conn::BarPosition::Top => Anchor::Top,
        eclipse_bar::conn::BarPosition::Bottom => Anchor::Bottom,
    };

    let mut builder =
        iced_layershell::build_pattern::daemon(app::App::new, namespace, app::update, view::view)
            .layer_settings(LayerShellSettings {
                anchor: edge | Anchor::Left | Anchor::Right,
                layer: Layer::Top,
                // Width 0 means "as wide as the output"; the height is also
                // the exclusive zone, so a window opened afterwards starts
                // below the bar rather than under it.
                size: Some((0, HEIGHT)),
                exclusive_zone: HEIGHT as i32,
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
/// Children are deliberately *not* detached into their own session: they are
/// meant to die with the supervisor, so stopping the unit stops every bar, and
/// a vanished monitor's child can be killed and reaped here rather than
/// outliving its output.
fn supervise() {
    use std::collections::HashMap;

    let mut conn = eclipse_bar::conn::Conn::new();
    let mut children: HashMap<String, std::process::Child> = HashMap::new();
    let exe = std::env::current_exe().unwrap_or_else(|_| "eclipse-bar".into());
    // The supervisor holds the subscription itself: an output event is the
    // one thing that means the set of monitors may have moved.
    let mut client = None;

    loop {
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
                match std::process::Command::new(&exe).arg("--output").arg(name).spawn() {
                    Ok(child) => {
                        children.insert(name.clone(), child);
                    }
                    Err(e) => eprintln!("eclipse-bar: cannot start a bar on {name}: {e}"),
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
