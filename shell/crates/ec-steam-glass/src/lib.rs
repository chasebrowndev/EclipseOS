// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-steam-glass` — the injector half of the steam-glass add-on (ADR 0066).
//!
//! Steam's client UI is a Chromium Embedded Framework app. With its DevTools
//! port on, a local process can evaluate script in each of its pages; this
//! crate does that with the add-on's stylesheet and recolouring script, and
//! keeps doing it as pages come, reload and go. It adds no capability: it
//! styles pixels Steam already draws, in a process of its own.

pub mod cdp;

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::json;

use cdp::{Target, Ws};

const CSS_FILE: &str = "steam-glass.css";
const JS_FILE: &str = "steam-glass.js";

/// Steam's own UI: its windows and menus are created as
/// `about:blank?createflags=..` and fill themselves in, and its shared context
/// is served from `steamloopback.host`. Web content (the store, the community)
/// has a real https URL, is other people's site, and is left alone.
pub fn is_steam_ui(url: &str) -> bool {
    url.starts_with("about:blank?createflags=") || url.starts_with("https://steamloopback.host/")
}

/// How the theme paints. `Glass` leaves tints translucent for the
/// compositor's blur to show through; `Backed` composites them over the
/// desktop base, for a window the compositor cannot see through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    Glass,
    #[default]
    Backed,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Glass => "glass",
            Mode::Backed => "backed",
        }
    }
}

/// The script that goes into a page: the stylesheet bound to `CSS` and the
/// mode to `MODE`, then the add-on's script, which installs it and replaces
/// any earlier install.
pub fn bundle_source(css: &str, js: &str, mode: Mode) -> String {
    format!(
        "(()=>{{const CSS={};const MODE={:?};\n{js}\n}})();",
        json!(css),
        mode.name()
    )
}

/// A loaded bundle and the file stamps it came from.
struct Bundle {
    source: String,
    stamp: [Option<SystemTime>; 2],
}

fn load(dir: &Path, mode: Mode) -> io::Result<Bundle> {
    let (css, js) = (dir.join(CSS_FILE), dir.join(JS_FILE));
    let mtime = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    Ok(Bundle {
        source: bundle_source(
            &std::fs::read_to_string(&css)?,
            &std::fs::read_to_string(&js)?,
            mode,
        ),
        stamp: [mtime(&css), mtime(&js)],
    })
}

fn stamp(dir: &Path) -> [Option<SystemTime>; 2] {
    let mtime = |n: &str| std::fs::metadata(dir.join(n)).and_then(|m| m.modified()).ok();
    [mtime(CSS_FILE), mtime(JS_FILE)]
}

/// Registers `source` for every future document in the page and runs it in
/// the current one. Returns the registration id, to remove on the next change.
fn install(ws: &mut Ws, source: &str, previous: Option<&str>) -> io::Result<String> {
    if let Some(id) = previous {
        ws.call(
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({ "identifier": id }),
        )?;
    }
    let added = ws.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": source }),
    )?;
    ws.call("Runtime.evaluate", json!({ "expression": source }))?;
    Ok(added["identifier"].as_str().unwrap_or_default().to_owned())
}

/// Styles one page and keeps it styled until the page's connection closes.
/// Registrations die with the DevTools session, which is why this holds the
/// connection open rather than injecting and leaving.
pub fn serve(port: u16, target: &Target, assets: &Path, mode: Mode) -> io::Result<()> {
    let mut ws = Ws::connect(port, &target.ws_path)?;
    let mut bundle = load(assets, mode)?;
    let mut id = install(&mut ws, &bundle.source, None)?;
    loop {
        if ws.readable(Duration::from_secs(1))? {
            ws.recv_text()?; // an event; nothing here subscribes to any
        } else if stamp(assets) != bundle.stamp {
            // A half-written file is retried on the next tick.
            if let Ok(next) = load(assets, mode) {
                id = install(&mut ws, &next.source, Some(&id))?;
                bundle = next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_steams_own_origin_is_styled() {
        assert!(is_steam_ui(
            "https://steamloopback.host/index.html?LANGUAGE=english"
        ));
        assert!(!is_steam_ui("https://store.steampowered.com/"));
        assert!(is_steam_ui("about:blank?createflags=18&minwidth=1010"));
        assert!(!is_steam_ui("about:blank"));
        assert!(!is_steam_ui("https://steamloopback.host.evil.example/"));
        assert!(!is_steam_ui("data:text/html,<body></body>"));
    }

    #[test]
    fn the_bundle_binds_the_stylesheet_as_a_json_string() {
        let src = bundle_source("a{b:\"c\"}\n", "install(CSS);", Mode::Glass);
        assert!(src.starts_with("(()=>{const CSS=\"a{b:\\\"c\\\"}\\n\";"));
        assert!(src.contains("const MODE=\"glass\";"));
        assert!(src.ends_with("install(CSS);\n})();"));
        assert!(bundle_source("", "", Mode::default()).contains("const MODE=\"backed\";"));
    }
}
