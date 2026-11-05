// SPDX-License-Identifier: AGPL-3.0-only
//! The login screen's customisation (ADR 0089). **TCB.**
//!
//! Settings are parsed strictly and rendered over the shipped regreet files, all
//! of it pure. [`apply`] is the root helper's half and writes only what it
//! serialised or encoded itself.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use ec_theme::{Rgba, Seeds};

/// The machine's choices. Root-owned; `greeter` only reads it (D-01 §4.1).
pub const STATE_DIR: &str = "/var/lib/eclipse/greeter";
pub const SETTINGS_FILE: &str = "settings.toml";
pub const WALLPAPER_FILE: &str = "wallpaper.png";

pub const MAX_GREETING: usize = 120;
pub const MAX_SETTINGS: usize = 4096;
pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2160;

/// A preset, never a free format: a bad strftime string could break the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Time24,
    Time12,
    Date24,
    Date12,
}

impl Clock {
    fn name(self) -> &'static str {
        match self {
            Clock::Time24 => "24h",
            Clock::Time12 => "12h",
            Clock::Date24 => "24h-date",
            Clock::Date12 => "12h-date",
        }
    }

    fn from_name(name: &str) -> Option<Clock> {
        [Clock::Time24, Clock::Time12, Clock::Date24, Clock::Date12]
            .into_iter()
            .find(|c| c.name() == name)
    }

    fn strftime(self) -> &'static str {
        match self {
            Clock::Time24 => "%H:%M",
            Clock::Time12 => "%I:%M %p",
            Clock::Date24 => "%a %d %b  %H:%M",
            Clock::Date12 => "%a %d %b  %I:%M %p",
        }
    }
}

/// Opaque `0xrrggbb` theme colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub base: u32,
    pub ink: u32,
    pub accent: u32,
    pub accent_text: u32,
}

impl Theme {
    fn seeds(self) -> Seeds {
        Seeds {
            base: Rgba::rgb(self.base),
            ink: Rgba::rgb(self.ink),
            accent: Rgba::rgb(self.accent),
            accent_text: Some(Rgba::rgb(self.accent_text)),
            frost_tint: Rgba::rgb(self.base),
            rim: 0.0,
        }
    }
}

/// Everything unset is the shipped look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Show `wallpaper.png` rather than the shipped image.
    pub wallpaper: bool,
    pub theme: Option<Theme>,
    pub greeting: Option<String>,
    pub clock: Option<Clock>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The settings file or the helper's input is not what we write.
    Invalid(&'static str),
    /// The shipped file lacks a line rendering needs.
    Shipped(&'static str),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(what) => write!(f, "invalid: {what}"),
            Error::Shipped(what) => write!(f, "shipped file has no {what}"),
            Error::Io(what) => write!(f, "{what}"),
        }
    }
}

fn io(what: &str, e: impl fmt::Display) -> Error {
    Error::Io(format!("{what}: {e}"))
}

fn hex(c: u32) -> String {
    format!("#{c:06x}")
}

fn parse_hex(s: &str) -> Option<u32> {
    let digits = s.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// Printable, one line, and short enough for the card.
fn greeting_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= MAX_GREETING && !s.chars().any(char::is_control)
}

/// A TOML basic string. `greeting_ok` has ruled out control characters, so `\`
/// and `"` are all that needs escaping.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return None,
            '\\' => match chars.next()? {
                c @ ('\\' | '"') => out.push(c),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

impl Settings {
    /// Strict: an unknown or repeated key, or a bad value, fails the whole file.
    pub fn parse(text: &str) -> Result<Settings, Error> {
        if text.len() > MAX_SETTINGS {
            return Err(Error::Invalid("too long"));
        }
        let mut s = Settings::default();
        let (mut wallpaper, mut greeting, mut clock) = (false, false, false);
        let (mut base, mut ink, mut accent, mut accent_text) = (None, None, None, None);
        let colour = |slot: &mut Option<u32>, v: &str| -> Result<(), Error> {
            let c = unquote(v).and_then(|c| parse_hex(&c));
            if slot.is_some() || c.is_none() {
                return Err(Error::Invalid("colour"));
            }
            *slot = c;
            Ok(())
        };
        for line in text.lines() {
            let (key, value) = line.split_once(" = ").ok_or(Error::Invalid("line"))?;
            match key {
                "wallpaper" if !wallpaper => {
                    wallpaper = true;
                    s.wallpaper = match value {
                        "true" => true,
                        "false" => false,
                        _ => return Err(Error::Invalid("wallpaper")),
                    };
                }
                "greeting" if !greeting => {
                    greeting = true;
                    let g = unquote(value).filter(|g| greeting_ok(g));
                    s.greeting = Some(g.ok_or(Error::Invalid("greeting"))?);
                }
                "clock" if !clock => {
                    clock = true;
                    let c = unquote(value).and_then(|c| Clock::from_name(&c));
                    s.clock = Some(c.ok_or(Error::Invalid("clock"))?);
                }
                "base" => colour(&mut base, value)?,
                "ink" => colour(&mut ink, value)?,
                "accent" => colour(&mut accent, value)?,
                "accent_text" => colour(&mut accent_text, value)?,
                _ => return Err(Error::Invalid("key")),
            }
        }
        s.theme = match (base, ink, accent, accent_text) {
            (None, None, None, None) => None,
            (Some(base), Some(ink), Some(accent), Some(accent_text)) => {
                let theme = Theme {
                    base,
                    ink,
                    accent,
                    accent_text,
                };
                ec_theme::validate(&theme.seeds()).map_err(|_| Error::Invalid("theme contrast"))?;
                Some(theme)
            }
            _ => return Err(Error::Invalid("partial theme")),
        };
        Ok(s)
    }

    /// The canonical form, and the only text root ever writes.
    pub fn to_text(&self) -> String {
        let mut out = format!("wallpaper = {}\n", self.wallpaper);
        if let Some(t) = self.theme {
            out += &format!(
                "base = \"{}\"\nink = \"{}\"\naccent = \"{}\"\naccent_text = \"{}\"\n",
                hex(t.base),
                hex(t.ink),
                hex(t.accent),
                hex(t.accent_text)
            );
        }
        if let Some(g) = &self.greeting {
            out += &format!("greeting = {}\n", quote(g));
        }
        if let Some(c) = self.clock {
            out += &format!("clock = \"{}\"\n", c.name());
        }
        out
    }
}

fn replace_key(text: &str, key: &str, value: &str) -> Result<String, Error> {
    let prefix = format!("{key} = ");
    if text.lines().filter(|l| l.starts_with(&prefix)).count() != 1 {
        return Err(Error::Shipped("config key"));
    }
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with(&prefix) {
                format!("{prefix}{value}")
            } else {
                l.to_owned()
            }
        })
        .collect();
    Ok(lines.join("\n") + "\n")
}

/// `regreet.toml`: the shipped file, with what the settings change.
pub fn render_config(shipped: &str, s: &Settings, wallpaper: Option<&Path>) -> Result<String, Error> {
    let mut out = shipped.to_owned();
    if let Some(path) = wallpaper {
        let path = path
            .to_str()
            .filter(|p| greeting_ok(p))
            .ok_or(Error::Invalid("path"))?;
        out = replace_key(&out, "path", &quote(path))?;
    }
    if let Some(g) = &s.greeting {
        out = replace_key(&out, "greeting_msg", &quote(g))?;
    }
    if let Some(c) = s.clock {
        out += &format!("\n[widget.clock]\nformat = \"{}\"\n", c.strftime());
    }
    Ok(out)
}

fn css(c: Rgba) -> String {
    let byte = |v: f32| (v * 255.0).round() as u8;
    format!(
        "rgba({}, {}, {}, {:.3})",
        byte(c.0[0]),
        byte(c.0[1]),
        byte(c.0[2]),
        c.0[3]
    )
}

/// `regreet.css`: the shipped file with its colour definitions replaced. The
/// rest is written in terms of them, so a package update to the shape still
/// applies.
pub fn render_style(shipped: &str, theme: Option<Theme>) -> Result<String, Error> {
    let Some(theme) = theme else {
        return Ok(shipped.to_owned());
    };
    let p = ec_theme::derive(&theme.seeds());
    let defines = [
        ("base", hex(theme.base)),
        ("raised", css(p.surface_1.with_alpha(0.94))),
        ("accent", hex(theme.accent)),
        ("accent_text", hex(theme.accent_text)),
        ("on_accent", css(p.on_accent)),
        ("ink", hex(theme.ink)),
        ("danger", css(p.danger)),
    ];
    let mut found = [false; 7];
    let mut out = String::with_capacity(shipped.len());
    for line in shipped.lines() {
        let name = line
            .strip_prefix("@define-color ")
            .and_then(|l| l.split_whitespace().next());
        match name.and_then(|n| defines.iter().position(|(d, _)| *d == n)) {
            Some(i) => {
                found[i] = true;
                out += &format!("@define-color {} {};\n", defines[i].0, defines[i].1);
            }
            None => {
                out += line;
                out.push('\n');
            }
        }
    }
    if found.contains(&false) {
        return Err(Error::Shipped("colour definition"));
    }
    Ok(out)
}

/// What the helper reads from stdin: three little-endian `u32`s (settings
/// length, width, height), the settings, then `width * height * 4` bytes of RGBA.
/// A zero width and height means no new image.
#[derive(Debug, PartialEq, Eq)]
pub struct Input {
    pub settings: Settings,
    /// Width, height and pixels.
    pub image: Option<(u32, u32, Vec<u8>)>,
}

const HEADER: usize = 12;

/// Longest input that can be valid, so the helper can cap its read.
pub const MAX_INPUT: u64 = (HEADER + MAX_SETTINGS) as u64 + (MAX_WIDTH as u64 * MAX_HEIGHT as u64 * 4);

fn le(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let b = bytes.get(at..at + 4).ok_or(Error::Invalid("header"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl Input {
    pub fn parse(mut bytes: Vec<u8>) -> Result<Input, Error> {
        let len = le(&bytes, 0)? as usize;
        let (w, h) = (le(&bytes, 4)?, le(&bytes, 8)?);
        if len > MAX_SETTINGS {
            return Err(Error::Invalid("settings too long"));
        }
        let none = w == 0 && h == 0;
        if !none && (w == 0 || h == 0 || w > MAX_WIDTH || h > MAX_HEIGHT) {
            return Err(Error::Invalid("image size"));
        }
        let pixels = w as usize * h as usize * 4;
        if bytes.len() != HEADER + len + pixels {
            return Err(Error::Invalid("length"));
        }
        let text = std::str::from_utf8(&bytes[HEADER..HEADER + len]).map_err(|_| Error::Invalid("utf-8"))?;
        let settings = Settings::parse(text)?;
        if !settings.wallpaper && !none {
            return Err(Error::Invalid("image without wallpaper"));
        }
        let image = (!none).then(|| (w, h, bytes.split_off(HEADER + len)));
        Ok(Input { settings, image })
    }
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| io("png", e))?;
    writer.write_image_data(rgba).map_err(|e| io("png", e))?;
    writer.finish().map_err(|e| io("png", e))?;
    Ok(out)
}

/// Whether `bytes` is a complete PNG this crate could have written.
pub fn png_ok(bytes: &[u8]) -> bool {
    let Ok(mut reader) = png::Decoder::new(std::io::Cursor::new(bytes)).read_info() else {
        return false;
    };
    let info = reader.info();
    if info.width > MAX_WIDTH || info.height > MAX_HEIGHT {
        return false;
    }
    let Some(size) = reader.output_buffer_size() else {
        return false;
    };
    let mut buf = vec![0; size];
    reader.next_frame(&mut buf).is_ok()
}

/// `dir` must be a real directory owned by `owner`; made if missing.
fn state_dir(dir: &Path, owner: u32) -> Result<(), Error> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .map_err(|e| io("state dir", e))?;
    let meta = fs::symlink_metadata(dir).map_err(|e| io("state dir", e))?;
    if !meta.is_dir() || meta.uid() != owner {
        return Err(Error::Invalid("state dir owner"));
    }
    Ok(())
}

/// Write, sync and rename, never following a link.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let tmp: PathBuf = dir.join(format!(".{name}.{}", std::process::id()));
    // Left by a crash, under a pid that has come round again.
    remove(&tmp)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|e| io("create", e))?;
    let done = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644)))
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp, dir.join(name)));
    if let Err(e) = done {
        let _ = fs::remove_file(&tmp);
        return Err(io("write", e));
    }
    Ok(())
}

fn remove(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io("remove", e)),
    }
}

/// Store `input`. A new image goes before the settings and an old one is removed
/// after them, so settings never name one that is missing. The runner checks it
/// decodes anyway.
pub fn apply(dir: &Path, owner: u32, input: &Input) -> Result<(), Error> {
    state_dir(dir, owner)?;
    match &input.image {
        Some((w, h, rgba)) => write_atomic(dir, WALLPAPER_FILE, &encode_png(*w, *h, rgba)?)?,
        None if input.settings.wallpaper => {
            let meta = fs::symlink_metadata(dir.join(WALLPAPER_FILE));
            if !meta.is_ok_and(|m| m.is_file()) {
                return Err(Error::Invalid("no wallpaper stored"));
            }
        }
        None => {}
    }
    write_atomic(dir, SETTINGS_FILE, input.settings.to_text().as_bytes())?;
    if input.settings.wallpaper {
        return Ok(());
    }
    remove(&dir.join(WALLPAPER_FILE))
}

/// Back to the shipped look.
pub fn reset(dir: &Path, owner: u32) -> Result<(), Error> {
    state_dir(dir, owner)?;
    remove(&dir.join(SETTINGS_FILE))?;
    remove(&dir.join(WALLPAPER_FILE))
}

/// Read and render the customised files, or `None` if nothing is customised.
/// Returns config and style text.
pub fn prepare(
    dir: &Path,
    shipped_config: &str,
    shipped_style: &str,
) -> Result<Option<(String, String)>, Error> {
    let text = match fs::read_to_string(dir.join(SETTINGS_FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io("settings", e)),
    };
    let s = Settings::parse(&text)?;
    let wallpaper = dir.join(WALLPAPER_FILE);
    if s.wallpaper {
        let bytes = fs::read(&wallpaper).map_err(|e| io("wallpaper", e))?;
        if !png_ok(&bytes) {
            return Err(Error::Invalid("wallpaper does not decode"));
        }
    }
    let config = render_config(shipped_config, &s, s.wallpaper.then_some(wallpaper.as_path()))?;
    Ok(Some((config, render_style(shipped_style, s.theme)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: Theme = Theme {
        base: 0x0b0906,
        ink: 0xffffff,
        accent: 0x4ca3ff,
        accent_text: 0x7fbcff,
    };
    const CONFIG: &str = include_str!("../../../../packaging/greetd/regreet.toml");
    const STYLE: &str = include_str!("../../../../packaging/greetd/regreet.css");

    fn full() -> Settings {
        Settings {
            wallpaper: true,
            theme: Some(THEME),
            greeting: Some("Hello \"you\" \\ <b>there</b>".into()),
            clock: Some(Clock::Date12),
        }
    }

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ec-greeter-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn uid() -> u32 {
        // SAFETY: `getuid` is always safe; it cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }

    fn framed(settings: &str, w: u32, h: u32, pixels: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(settings.len() as u32).to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(settings.as_bytes());
        out.resize(out.len() + pixels, 7);
        out
    }

    #[test]
    fn settings_round_trip() {
        assert_eq!(Settings::parse(&full().to_text()), Ok(full()));
        assert_eq!(
            Settings::parse(&Settings::default().to_text()),
            Ok(Settings::default())
        );
    }

    #[test]
    fn anything_unexpected_fails_the_whole_file() {
        for bad in [
            "nonsense",
            "unknown = \"x\"",
            "wallpaper = maybe",
            "wallpaper = true\nwallpaper = false",
            "clock = \"%H:%M\"",
            "clock = \"24h\"\nclock = \"12h\"",
            "greeting = \"\"",
            "greeting = \"a\tb\"",
            "greeting = \"a\"b\"",
            "greeting = \"a\\nb\"",
            "base = \"#0b0906\"",
            "base = \"red\"\nink = \"#ffffff\"\naccent = \"#ffffff\"\naccent_text = \"#ffffff\"",
        ] {
            assert!(Settings::parse(bad).is_err(), "{bad}");
        }
        let long = format!("greeting = \"{}\"", "a".repeat(MAX_GREETING + 1));
        assert_eq!(Settings::parse(&long), Err(Error::Invalid("greeting")));
        assert!(Settings::parse(&"a".repeat(MAX_SETTINGS + 1)).is_err());
    }

    #[test]
    fn a_theme_that_fails_contrast_is_refused() {
        let same = "base = \"#808080\"\nink = \"#808080\"\naccent = \"#808080\"\naccent_text = \"#808080\"";
        assert_eq!(Settings::parse(same), Err(Error::Invalid("theme contrast")));
    }

    #[test]
    fn nothing_set_leaves_the_shipped_files_alone() {
        let s = Settings::default();
        assert_eq!(render_config(CONFIG, &s, None).unwrap(), CONFIG);
        assert_eq!(render_style(STYLE, None).unwrap(), STYLE);
    }

    #[test]
    fn the_config_carries_each_setting_as_toml() {
        let out = render_config(CONFIG, &full(), Some(Path::new("/var/lib/w.png"))).unwrap();
        assert!(out.contains("path = \"/var/lib/w.png\"\n"));
        assert!(out.contains("greeting_msg = \"Hello \\\"you\\\" \\\\ <b>there</b>\"\n"));
        assert!(out.ends_with("[widget.clock]\nformat = \"%a %d %b  %I:%M %p\"\n"));
        assert!(!out.contains("greeter.png\""));
    }

    #[test]
    fn the_style_replaces_every_definition_it_must() {
        let out = render_style(STYLE, Some(THEME)).unwrap();
        assert!(out.contains("@define-color accent #4ca3ff;\n"));
        assert!(out.contains("@define-color ink #ffffff;\n"));
        assert!(!out.contains("#f2c33c"));
        assert!(
            !out.contains("242, 195, 60"),
            "a literal accent outlives the theme"
        );
        assert!(!out.contains("#0b0906;\n    "));
    }

    #[test]
    fn a_shipped_file_missing_a_line_is_an_error() {
        assert!(render_style("window {}\n", Some(THEME)).is_err());
        assert!(render_config("[x]\n", &full(), None).is_err());
    }

    #[test]
    fn the_helpers_input_is_exact() {
        let text = Settings {
            wallpaper: true,
            ..Default::default()
        }
        .to_text();
        let good = framed(&text, 2, 1, 8);
        let input = Input::parse(good.clone()).unwrap();
        assert_eq!(input.image, Some((2, 1, vec![7; 8])));
        let mut extra = good.clone();
        extra.push(0);
        assert!(Input::parse(extra).is_err());
        assert!(Input::parse(good[..good.len() - 1].to_vec()).is_err());
        assert!(Input::parse(framed(&text, 3841, 1, 3841 * 4)).is_err());
        assert!(Input::parse(framed(&text, 1, 2161, 2161 * 4)).is_err());
        assert!(Input::parse(framed(&text, 0, 1, 0)).is_err());
        assert!(Input::parse(framed(&text, u32::MAX, u32::MAX, 0)).is_err());
        assert!(Input::parse(framed("wallpaper = false\n", 1, 1, 4)).is_err());
        assert!(Input::parse(vec![1, 2, 3]).is_err());
    }

    #[test]
    fn apply_stores_what_prepare_reads_and_reset_clears_it() {
        let dir = tempdir("apply");
        let image = Some((2, 1, vec![9; 8]));
        apply(
            &dir,
            uid(),
            &Input {
                settings: full(),
                image,
            },
        )
        .unwrap();
        let (config, style) = prepare(&dir, CONFIG, STYLE).unwrap().unwrap();
        assert!(config.contains(dir.join(WALLPAPER_FILE).to_str().unwrap()));
        assert!(style.contains("#4ca3ff"));
        // Keeping the stored image takes no new one.
        apply(
            &dir,
            uid(),
            &Input {
                settings: full(),
                image: None,
            },
        )
        .unwrap();
        assert!(dir.join(WALLPAPER_FILE).exists());
        let plain = Settings::default();
        apply(
            &dir,
            uid(),
            &Input {
                settings: plain,
                image: None,
            },
        )
        .unwrap();
        assert!(!dir.join(WALLPAPER_FILE).exists());
        reset(&dir, uid()).unwrap();
        assert_eq!(prepare(&dir, CONFIG, STYLE), Ok(None));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_refuses_a_wallpaper_it_does_not_have_and_a_foreign_dir() {
        let dir = tempdir("refuse");
        let input = Input {
            settings: full(),
            image: None,
        };
        assert!(apply(&dir, uid(), &input).is_err());
        assert!(!dir.join(SETTINGS_FILE).exists());
        assert!(apply(&dir, uid() + 1, &input).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_store_is_an_error_for_the_runner_to_fall_back_on() {
        let dir = tempdir("broken");
        assert_eq!(prepare(&dir, CONFIG, STYLE), Ok(None));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SETTINGS_FILE), "garbage").unwrap();
        assert!(prepare(&dir, CONFIG, STYLE).is_err());
        fs::write(dir.join(SETTINGS_FILE), "wallpaper = true\n").unwrap();
        assert!(prepare(&dir, CONFIG, STYLE).is_err(), "no image");
        let png = encode_png(2, 1, &[9; 8]).unwrap();
        fs::write(dir.join(WALLPAPER_FILE), &png[..png.len() / 2]).unwrap();
        assert!(prepare(&dir, CONFIG, STYLE).is_err(), "truncated");
        fs::write(dir.join(WALLPAPER_FILE), &png).unwrap();
        assert!(prepare(&dir, CONFIG, STYLE).unwrap().is_some());
        fs::remove_dir_all(&dir).unwrap();
    }
}
