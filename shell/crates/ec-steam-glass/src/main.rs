// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-steam-glass [--glass] [--port N] [--assets DIR]` — styles Steam's UI with the
//! Eclipse glass theme (opaque-backed unless `--glass`: Steam's window must
//! be see-through for that) for as long as Steam is running, and waits quietly
//! when it is not.

use std::collections::HashMap;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ec_steam_glass::{cdp, is_steam_ui, serve, Mode};

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_ASSETS: &str = "/usr/share/eclipse/steam-glass";

fn main() {
    let mut port = DEFAULT_PORT;
    let mut assets =
        PathBuf::from(std::env::var_os("EC_STEAM_GLASS_ASSETS").unwrap_or_else(|| DEFAULT_ASSETS.into()));
    let mut mode = Mode::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--glass" => mode = Mode::Glass,
            "--port" => {
                port = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--assets" => assets = args.next().unwrap_or_else(|| usage()).into(),
            _ => usage(),
        }
    }
    enable_steam_debugging();

    let mut live: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut waiting = false;
    loop {
        live.retain(|_, h| !h.is_finished());
        match cdp::list_targets(port) {
            Ok(targets) => {
                if waiting {
                    eprintln!("ec-steam-glass: Steam is up");
                    waiting = false;
                }
                for t in targets.into_iter().filter(|t| is_steam_ui(&t.url)) {
                    if live.contains_key(&t.id) {
                        continue;
                    }
                    let assets = assets.clone();
                    let id = t.id.clone();
                    live.insert(
                        id,
                        thread::spawn(move || {
                            // A page closing ends its connection; that is the
                            // normal exit, so only the unexpected is logged.
                            if let Err(e) = serve(port, &t, &assets, mode) {
                                if e.kind() != std::io::ErrorKind::ConnectionAborted {
                                    eprintln!("ec-steam-glass: {} ({}): {e}", t.title, t.id);
                                }
                            }
                        }),
                    );
                }
            }
            Err(_) if !waiting => {
                eprintln!("ec-steam-glass: waiting for Steam's DevTools port {port}");
                waiting = true;
            }
            Err(_) => {}
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Steam opens its DevTools port at start-up when this marker sits in its
/// install directory, so it needs no launch flag. It takes effect the next
/// time Steam starts.
fn enable_steam_debugging() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let marker = PathBuf::from(home).join(".steam/steam/.cef-enable-remote-debugging");
    if marker.parent().is_some_and(|p| p.is_dir()) && !marker.exists() {
        if let Err(e) = std::fs::write(&marker, b"") {
            eprintln!("ec-steam-glass: cannot create {}: {e}", marker.display());
        }
    }
}

fn usage() -> ! {
    eprintln!("usage: ec-steam-glass [--glass] [--port N] [--assets DIR]");
    std::process::exit(2);
}
