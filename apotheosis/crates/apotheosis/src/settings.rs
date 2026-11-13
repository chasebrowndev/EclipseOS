// SPDX-License-Identifier: AGPL-3.0-only
//! Configuration to runtime settings (T-10). Layers are loaded by `apoth-config`
//! (`/etc/eclipse/apotheosis.kdl` under `~/.config/eclipse/apotheosis.kdl`, defaults when
//! both are absent). Values are never logged, only counts and key paths of diagnostics.

use std::path::{Path, PathBuf};
use std::time::Duration;

use apoth_config::Config;
use apoth_config::fs::RealFs;
use apoth_config::load::default_input;
use apoth_config::reload::Reloader;
use apoth_font::config::{FontConfig, Ligatures, Style};
use apoth_grid::Limits;
use apoth_render::colour::TextBlending;
use apoth_render::cursor::CursorShape;
use apoth_render::surface::AlphaRequest;
use apoth_term::{CursorStyle, TermConfig};
use apoth_ui::frame::{TabPosition, TabStyle};
use apoth_wl::BlurPath;
use apoth_wl::window::DecorationPref;

/// The initial grid when the configuration names none (80x24).
pub(crate) const INITIAL_COLS: usize = 80;
/// The initial grid rows.
pub(crate) const INITIAL_ROWS: usize = 24;
/// Bounds of a configured initial grid.
const INITIAL_MIN: i64 = 10;
const INITIAL_MAX: i64 = 500;

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
    /// `window.opacity` when the file sets it; `None` is the computed default (T-08 §3): 0.82
    /// while the compositor blurs behind the window, 1 otherwise.
    pub(crate) opacity: Option<f32>,
    pub(crate) glass: GlassSettings,
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
    /// `colors.palette`: the 16 ANSI colours when set.
    pub(crate) ansi: Option<[(u8, u8, u8); 16]>,
    /// Initial grid `(rows, cols)`: `window.rows` / `window.columns` when the schema has
    /// them (it does not yet: spec gap), else 24x80.
    pub(crate) initial_size: (usize, usize),
    pub(crate) repeat_rate: Option<u32>,
    pub(crate) repeat_delay_ms: Option<u32>,
    /// `-o key=value` pairs and diagnostics counts, for the startup log line.
    pub(crate) diagnostics: usize,
}

/// The glass keys (T-08 §3-§4, T-09 §5): window material and the interface's own look.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GlassSettings {
    /// `window.blur-fallback opaque`: with no compositor blur the window is opaque; `tint`
    /// keeps the configured translucency under a tint.
    pub(crate) blur_fallback_opaque: bool,
    /// `window.alpha-mode`.
    pub(crate) alpha: AlphaRequest,
    /// `tabs.style` / `tabs.position`: read here; consumed by the tab strip surface (M18).
    pub(crate) tabs_style: TabStyle,
    pub(crate) tabs_position: TabPosition,
    /// `scrollbar.marks`: consumed by the scrollbar surface.
    pub(crate) scrollbar_marks: bool,
    /// `interface.font`: family of the interface's own text (None follows the terminal's).
    pub(crate) interface_font: Option<String>,
    /// `interface.font-size` over the 13 px body token, times `interface.scale`.
    pub(crate) interface_zoom: f32,
}

impl GlassSettings {
    pub(crate) fn from_config(c: &Config) -> GlassSettings {
        let alpha = match c.str("window.alpha-mode") {
            Some("opaque") => AlphaRequest::Opaque,
            Some("premultiplied") => AlphaRequest::PreMultiplied,
            // `postmultiplied` has no request of its own in the renderer; `auto` substitutes.
            _ => AlphaRequest::Auto,
        };
        let size = c.float("interface.font-size") as f32;
        let scale = c.float("interface.scale") as f32;
        GlassSettings {
            blur_fallback_opaque: c.str("window.blur-fallback") == Some("opaque"),
            alpha,
            tabs_style: match c.str("tabs.style") {
                Some("flat") => TabStyle::Flat,
                Some("hidden") => TabStyle::Hidden,
                _ => TabStyle::Floating,
            },
            tabs_position: match c.str("tabs.position") {
                Some("bottom") => TabPosition::Bottom,
                _ => TabPosition::Top,
            },
            scrollbar_marks: c.bool("scrollbar.marks"),
            interface_font: nonempty(c.str("interface.font")),
            interface_zoom: if size > 0.0 && scale > 0.0 {
                size / apoth_ui::tokens::size::BODY * scale
            } else {
                1.0
            },
        }
    }
}

/// Opacity of the window and the clear colour: the configured value, else 0.82 over a
/// compositor blur and 1 without (T-08 §3). With `blur-fallback opaque` and no blur the window
/// is opaque whatever was configured.
pub(crate) fn effective_opacity(
    configured: Option<f32>,
    glass: &GlassSettings,
    blur: Option<BlurPath>,
) -> f32 {
    let blurred = matches!(blur, Some(BlurPath::Ext | BlurPath::Kwin));
    match configured {
        _ if !blurred && glass.blur_fallback_opaque => 1.0,
        Some(o) => o.clamp(0.0, 1.0),
        None if blurred => AUTO_OPACITY,
        None => 1.0,
    }
}

/// `window.opacity`'s computed default while a blur is available (schema row doc).
pub(crate) const AUTO_OPACITY: f32 = 0.82;

/// What startup loaded: the settings, the config they came from (the baseline for the first
/// reload diff), the watch set and the error positions for the error bar.
#[derive(Debug)]
pub(crate) struct Startup {
    pub(crate) settings: Settings,
    /// The live config and the diagnostics of the last attempt (T-10 §4).
    pub(crate) reloader: apoth_config::reload::Reloader,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) launch: Vec<(String, String)>,
    pub(crate) errors: Vec<ErrorPos>,
}

/// Where a config error is. Never the offending text: values can be private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ErrorPos {
    pub(crate) file: Option<PathBuf>,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) key: Option<String>,
    /// The diagnostic's message. `apoth-config` messages never contain values.
    pub(crate) message: String,
}

/// Positions and messages of every error-severity diagnostic.
pub(crate) fn error_positions(diags: &[apoth_config::Diagnostic]) -> Vec<ErrorPos> {
    diags
        .iter()
        .filter(|d| d.is_error())
        .map(|d| ErrorPos {
            file: d.file.clone(),
            line: d.line,
            column: d.column,
            key: d.key.clone(),
            message: d.message.clone(),
        })
        .collect()
}

/// The standard file locations plus the launch overrides (re-applied on every reload).
pub(crate) fn load_input(launch: &[(String, String)]) -> apoth_config::LoadInput {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let mut input = default_input(&RealFs, &home, xdg.as_deref());
    input.launch = launch.to_vec();
    input
}

fn parse_ansi(list: &[String]) -> Option<[(u8, u8, u8); 16]> {
    if list.len() != 16 {
        return None;
    }
    let mut out = [(0, 0, 0); 16];
    for (o, s) in out.iter_mut().zip(list) {
        let c = apoth_config::lang::parse_color(s)?;
        *o = rgb3(c);
    }
    Some(out)
}

/// A configured grid dimension, or `default` when the key is absent or out of range.
fn initial_dim(c: &Config, path: &str, default: usize) -> usize {
    c.get(path)
        .and_then(apoth_config::Value::as_int)
        .filter(|n| (INITIAL_MIN..=INITIAL_MAX).contains(n))
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
}

fn rgb3(c: [u8; 4]) -> (u8, u8, u8) {
    (c[0], c[1], c[2])
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty()).map(str::to_owned)
}

impl Settings {
    /// Loads layers 1-7 for startup. A file that does not exist is not an error. Bad values
    /// keep their defaults (the tolerant result); first start with a broken file runs on
    /// defaults plus the error bar (T-10 §4).
    pub(crate) fn load(launch: Vec<(String, String)>) -> Startup {
        let input = load_input(&launch);
        let reloader = Reloader::start(&RealFs, &input);
        let errors = error_positions(reloader.diagnostics());
        let diagnostics = reloader.diagnostics().len();
        if diagnostics > 0 {
            tracing::warn!(
                diagnostics,
                errors = errors.len(),
                "configuration has diagnostics; run `apoth config validate`"
            );
        }
        let mut settings = Settings::from_config(reloader.config());
        settings.diagnostics = diagnostics;
        Startup {
            settings,
            files: reloader.files().to_vec(),
            reloader,
            launch,
            errors,
        }
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
        // `window.opacity` is Computed unless the file sets it: `None` here, resolved against
        // the compositor's blur by `effective_opacity`.
        let opacity = c
            .get("window.opacity")
            .and_then(|v| v.as_float())
            .map(|f| f as f32);
        let rate = c.int("keyboard.repeat-rate");
        let delay = c.millis("keyboard.repeat-delay");
        Settings {
            font,
            family_named,
            padding: (px("window.padding-x"), px("window.padding-y")),
            opacity,
            glass: GlassSettings::from_config(c),
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
            ansi: parse_ansi(c.list("colors.palette")),
            initial_size: (
                initial_dim(c, "window.rows", INITIAL_ROWS),
                initial_dim(c, "window.columns", INITIAL_COLS),
            ),
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
        assert_eq!(s.opacity, None, "unset opacity is computed");
        assert!((effective_opacity(s.opacity, &s.glass, None) - 1.0).abs() < f32::EPSILON);
        assert!(!s.family_named);
        assert_eq!(s.term_name, "xterm-apotheosis");
        assert_eq!(s.window_title(""), "Apotheosis");
        assert_eq!(s.window_title("x"), "x");
        assert_eq!(s.term.scrollback.lines, None);
        assert_eq!(s.initial_size, (INITIAL_ROWS, INITIAL_COLS));
        assert_eq!(s.ansi, None);
    }

    #[test]
    fn auto_opacity_follows_blur_capability() {
        let s = Settings::from_config(&Config::defaults());
        let eff = |b| effective_opacity(s.opacity, &s.glass, b);
        assert!((eff(Some(BlurPath::Ext)) - AUTO_OPACITY).abs() < f32::EPSILON);
        assert!((eff(Some(BlurPath::Kwin)) - AUTO_OPACITY).abs() < f32::EPSILON);
        assert!((eff(Some(BlurPath::Tint)) - 1.0).abs() < f32::EPSILON);
        assert!((eff(None) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn explicit_opacity_and_blur_fallback() {
        let mut c = Config::defaults();
        assert!(c.set_at(
            "window.opacity",
            apoth_config::Value::Float(0.5),
            apoth_config::model::Origin::builtin()
        ));
        let s = Settings::from_config(&c);
        assert!((effective_opacity(s.opacity, &s.glass, Some(BlurPath::Tint)) - 0.5).abs() < 1e-6);
        let mut g = s.glass.clone();
        g.blur_fallback_opaque = true;
        assert!((effective_opacity(s.opacity, &g, Some(BlurPath::Tint)) - 1.0).abs() < 1e-6);
        assert!((effective_opacity(s.opacity, &g, Some(BlurPath::Ext)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn glass_keys_are_read() {
        use apoth_config::Value;
        use apoth_config::model::Origin;
        let mut c = Config::defaults();
        let mut set = |k: &str, v: Value| assert!(c.set_at(k, v, Origin::builtin()), "{k}");
        set("window.alpha-mode", Value::Str("opaque".into()));
        set("window.blur-fallback", Value::Str("opaque".into()));
        set("tabs.style", Value::Str("flat".into()));
        set("tabs.position", Value::Str("bottom".into()));
        set("scrollbar.marks", Value::Bool(false));
        set("interface.font-size", Value::Float(26.0));
        let g = Settings::from_config(&c).glass;
        assert_eq!(g.alpha, AlphaRequest::Opaque);
        assert!(g.blur_fallback_opaque);
        assert_eq!(g.tabs_style, TabStyle::Flat);
        assert_eq!(g.tabs_position, TabPosition::Bottom);
        assert!(!g.scrollbar_marks);
        assert!((g.interface_zoom - 2.0).abs() < 1e-6);
        let d = Settings::from_config(&Config::defaults()).glass;
        assert_eq!(d.tabs_style, TabStyle::Floating);
        assert!(d.scrollbar_marks);
        assert!((d.interface_zoom - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ansi_palette_needs_sixteen_valid_colours() {
        let good: Vec<String> = (0..16).map(|i| format!("#0000{i:02x}")).collect();
        let p = parse_ansi(&good).unwrap();
        assert_eq!(p[15], (0, 0, 15));
        assert_eq!(parse_ansi(&good[..15]), None);
        let mut bad = good;
        bad[3] = "nope".into();
        assert_eq!(parse_ansi(&bad), None);
    }

    #[test]
    fn initial_dimension_falls_back() {
        let c = Config::defaults();
        // The schema has no such key yet: the default holds.
        assert_eq!(initial_dim(&c, "window.columns", 80), 80);
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
