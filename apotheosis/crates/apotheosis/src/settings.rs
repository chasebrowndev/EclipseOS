// SPDX-License-Identifier: AGPL-3.0-only
//! Configuration to runtime settings (T-10). Layers are loaded by `apoth-config`
//! (`/etc/eclipse/apotheosis.kdl` under `~/.config/eclipse/apotheosis.kdl`, defaults when
//! both are absent). Values are never logged, only counts and key paths of diagnostics.

use std::path::{Path, PathBuf};
use std::time::Duration;

use apoth_config::fs::RealFs;
use apoth_config::load::default_input;
use apoth_config::{Config, load};
use apoth_font::config::{FontConfig, Ligatures, Style};
use apoth_grid::Limits;
use apoth_render::colour::TextBlending;
use apoth_render::cursor::CursorShape;
use apoth_term::{CursorStyle, TermConfig};
use apoth_wl::window::DecorationPref;

/// The initial grid (no `window.columns` key exists in the schema: spec gap).
pub(crate) const INITIAL_COLS: usize = 80;
/// The initial grid rows.
pub(crate) const INITIAL_ROWS: usize = 24;

/// Shell launch settings (`shell.*`).
#[derive(Clone, Debug)]
pub(crate) struct ShellSettings {
    pub(crate) program: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) login: bool,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: Vec<(String, String)>,
}

/// Everything the binary reads from the configuration.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) font: FontConfig,
    /// The regular family was named explicitly (not the `monospace` default).
    pub(crate) family_named: bool,
    pub(crate) padding: (u32, u32),
    pub(crate) opacity: f32,
    pub(crate) blending: TextBlending,
    pub(crate) bold_bright: bool,
    pub(crate) decorations: DecorationPref,
    pub(crate) title_template: String,
    pub(crate) shell: ShellSettings,
    pub(crate) term_name: String,
    pub(crate) term: TermConfig,
    pub(crate) cursor_shape: CursorShape,
    pub(crate) unfocused_cursor_hidden: bool,
    pub(crate) allow_software: bool,
    pub(crate) echo_window: Duration,
    pub(crate) sync_timeout: Duration,
    pub(crate) bell_audible: bool,
    pub(crate) repeat_rate: Option<u32>,
    pub(crate) repeat_delay_ms: Option<u32>,
    /// `-o key=value` pairs and diagnostics counts, for the startup log line.
    pub(crate) diagnostics: usize,
}

fn rgb3(c: [u8; 4]) -> (u8, u8, u8) {
    (c[0], c[1], c[2])
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty()).map(str::to_owned)
}

impl Settings {
    /// Loads layers 1-7. A file that does not exist is not an error. Bad values keep their
    /// defaults (the tolerant result) and are counted.
    pub(crate) fn load(launch: Vec<(String, String)>) -> Settings {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let mut input = default_input(&RealFs, &home, xdg.as_deref());
        input.launch = launch;
        let result = load(&RealFs, &input, None);
        let diagnostics = result.diagnostics.len();
        if diagnostics > 0 {
            tracing::warn!(
                diagnostics,
                "configuration has diagnostics; run `apoth config validate`"
            );
        }
        let mut s = Settings::from_config(&result.config);
        s.diagnostics = diagnostics;
        s
    }

    pub(crate) fn from_config(c: &Config) -> Settings {
        let mut font = FontConfig::default();
        let fam = nonempty(c.str("font.family"));
        let family_named = fam.as_deref().is_some_and(|f| f != "monospace");
        if family_named {
            font.styles[Style::Regular.index()].family = fam;
        }
        for (path, style) in [
            ("font.bold-family", Style::Bold),
            ("font.italic-family", Style::Italic),
            ("font.bold-italic-family", Style::BoldItalic),
        ] {
            font.styles[style.index()].family = nonempty(c.str(path));
        }
        font.size_pt = c.float("font.size") as f32;
        font.fallback = c.list("font.fallback").to_vec();
        font.ligatures = match c.str("font.ligatures") {
            Some("never") => Ligatures::Off,
            Some("always") => Ligatures::On,
            _ => Ligatures::NotUnderCursor,
        };
        let syn = c.bool("font.synthetic");
        font.synthetic.bold = syn;
        font.synthetic.italic = syn;

        let blending = match c.str("render.text-blending") {
            Some("native") => TextBlending::Native,
            Some("linear") => TextBlending::Linear,
            _ => TextBlending::LinearCorrected,
        };
        let decorations = match c.str("window.decorations") {
            Some("client") => DecorationPref::Client,
            Some("server") => DecorationPref::Server,
            Some("none") => DecorationPref::None,
            _ => DecorationPref::Auto,
        };
        let (shape_code, cursor_shape) = match c.str("cursor.style") {
            Some("underline") => (3u8, CursorShape::Underline),
            Some("bar") => (5u8, CursorShape::Beam),
            _ => (1u8, CursorShape::Block),
        };
        let blink = c.bool("cursor.blink");
        // DECSCUSR: odd = blinking, even = steady.
        let shape = if blink { shape_code } else { shape_code + 1 };

        let mut term = TermConfig {
            sync_timeout: Duration::from_millis(c.millis("render.sync-timeout")),
            osc52_write: c.bool("clipboard.osc52"),
            osc52_max: usize::try_from(c.bytes("clipboard.osc52-max")).unwrap_or(1 << 20),
            ..TermConfig::default()
        };
        let lines = c.int("scrollback.lines");
        term.scrollback = Limits {
            bytes: usize::try_from(c.bytes("scrollback.bytes")).unwrap_or(64 << 20),
            lines: usize::try_from(lines).ok().filter(|n| *n > 0),
        };
        term.cursor = CursorStyle { shape, blink };
        if let Some(fg) = c.color("colors.foreground") {
            term.fg = rgb3(fg);
        }
        if let Some(bg) = c.color("colors.background") {
            term.bg = rgb3(bg);
        }
        if let Some(cc) = c.color("colors.cursor") {
            term.cursor_color = rgb3(cc);
        } else {
            term.cursor_color = term.fg;
        }

        let env = c
            .get("shell.env")
            .and_then(|v| v.as_map())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.first().map(|v| (k.clone(), v.clone())))
                    .collect()
            })
            .unwrap_or_default();
        let shell = ShellSettings {
            program: nonempty(c.str("shell.program")),
            args: c.list("shell.args").to_vec(),
            login: c.bool("shell.login"),
            cwd: nonempty(c.str("shell.cwd")).map(PathBuf::from),
            env,
        };
        let px = |p: &str| u32::try_from(c.int(p).max(0)).unwrap_or(0);
        // `window.opacity` defaults to Computed (0.82 with blur): no blur path yet, so 1.
        let opacity = c
            .get("window.opacity")
            .and_then(|v| v.as_float())
            .map_or(1.0, |f| f as f32);
        let rate = c.int("keyboard.repeat-rate");
        let delay = c.millis("keyboard.repeat-delay");
        Settings {
            font,
            family_named,
            padding: (px("window.padding-x"), px("window.padding-y")),
            opacity: opacity.clamp(0.0, 1.0),
            blending,
            bold_bright: c.bool("colors.bold-is-bright"),
            decorations,
            title_template: c.str("window.title").unwrap_or("{title}").to_owned(),
            shell,
            term_name: c
                .str("terminal.term")
                .unwrap_or("xterm-apotheosis")
                .to_owned(),
            term,
            cursor_shape,
            unfocused_cursor_hidden: c.str("cursor.unfocused-style") == Some("none"),
            allow_software: c.bool("render.allow-software"),
            echo_window: Duration::from_millis(c.millis("render.echo-window")),
            sync_timeout: Duration::from_millis(c.millis("render.sync-timeout")),
            bell_audible: c.bool("bell.audible"),
            repeat_rate: u32::try_from(rate).ok().filter(|r| *r > 0),
            repeat_delay_ms: u32::try_from(delay).ok().filter(|d| *d > 0),
            diagnostics: 0,
        }
    }

    /// The window title for a terminal-set title (`{title}` is the only placeholder here).
    pub(crate) fn window_title(&self, title: &str) -> String {
        let t = if title.is_empty() {
            "Apotheosis"
        } else {
            title
        };
        self.title_template.replace("{title}", t)
    }
}

/// Shell resolution (CONFIG-41): `shell.program`, then `$SHELL`, then the passwd entry of the
/// real uid, then `/bin/sh`.
pub(crate) fn resolve_shell(configured: Option<&str>) -> String {
    if let Some(p) = configured {
        return p.to_owned();
    }
    if let Some(s) = std::env::var("SHELL").ok().filter(|s| !s.is_empty()) {
        return s;
    }
    passwd_shell(Path::new("/etc/passwd"), rustix::process::getuid().as_raw())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// The login shell of `uid` in a passwd-format file.
pub(crate) fn passwd_shell(path: &Path, uid: u32) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_passwd_shell(&text, uid)
}

pub(crate) fn parse_passwd_shell(text: &str, uid: u32) -> Option<String> {
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 7 && f[2].parse::<u32>().ok() == Some(uid) && !f[6].is_empty())
            .then(|| f[6].to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_without_files() {
        let s = Settings::from_config(&Config::defaults());
        assert_eq!(s.padding, (6, 6));
        assert!((s.opacity - 1.0).abs() < f32::EPSILON);
        assert!(!s.family_named);
        assert_eq!(s.term_name, "xterm-apotheosis");
        assert_eq!(s.window_title(""), "Apotheosis");
        assert_eq!(s.window_title("x"), "x");
        assert_eq!(s.term.scrollback.lines, None);
    }

    #[test]
    fn passwd_parsing() {
        let t = "root:x:0:0:root:/root:/bin/zsh\nme:x:1000:1000::/home/me:/usr/bin/fish\nbad:x:5\n";
        assert_eq!(
            parse_passwd_shell(t, 1000).as_deref(),
            Some("/usr/bin/fish")
        );
        assert_eq!(parse_passwd_shell(t, 0).as_deref(), Some("/bin/zsh"));
        assert_eq!(parse_passwd_shell(t, 5), None);
    }

    #[test]
    fn configured_shell_wins() {
        assert_eq!(resolve_shell(Some("/bin/dash")), "/bin/dash");
    }
}
