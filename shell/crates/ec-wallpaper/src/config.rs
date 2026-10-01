// SPDX-License-Identifier: AGPL-3.0-only
//! The `wallpaper` node, as `get_config` reports it.
//!
//! ```kdl
//! wallpaper {
//!     path "~/Pictures/x.png"   // optional
//!     mode "fill"               // fill | fit | center
//!     color "#0b0906ff"         // #rrggbb or #rrggbbaa
//!     output "DP-1" { path "..." mode "fit" color "#..." }
//! }
//! ```
//!
//! Every key is optional and every override is any subset. [`from_reply`] is
//! the only function that knows the reply's shape; everything else works on
//! [`Wallpaper`].

use std::collections::HashMap;
use std::path::PathBuf;

use iced::{Color, ContentFit};
use serde_json::Value;

/// How the picture meets the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Cover the output, cropping what overhangs.
    #[default]
    Fill,
    /// All of the picture, letterboxed in the colour.
    Fit,
    /// One image pixel per logical pixel, centred; cropped or bordered.
    Center,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        match s {
            "fill" => Some(Mode::Fill),
            "fit" => Some(Mode::Fit),
            "center" => Some(Mode::Center),
            _ => None,
        }
    }

    /// `Center` is not `ContentFit::None`: iced pins an unscaled image to the
    /// top-left of its bounds. The view centres it itself (`view::picture`).
    pub fn content_fit(self) -> ContentFit {
        match self {
            Mode::Fill => ContentFit::Cover,
            Mode::Fit | Mode::Center => ContentFit::Contain,
        }
    }
}

/// Some subset of the three keys: the global rows, or one override.
#[derive(Debug, Clone, Default, PartialEq)]
struct Partial {
    /// `Some("")` is an explicit "no picture", which an override may want.
    path: Option<String>,
    mode: Option<Mode>,
    color: Option<Color>,
}

impl Partial {
    /// Read the keys out of one object, skipping any that are absent, null or
    /// the wrong shape.
    fn read(obj: &Value) -> Partial {
        let s = |k: &str| obj.get(k).and_then(Value::as_str);
        Partial {
            path: s("path").map(str::to_owned),
            mode: s("mode").and_then(Mode::parse),
            color: s("color").and_then(parse_color),
        }
    }

    /// One `keys[]` row; `null` or a non-string leaves the field unset.
    fn set(&mut self, key: &str, value: &Value) {
        let Some(s) = value.as_str() else {
            return;
        };
        match key {
            "path" => self.path = Some(s.to_owned()),
            "mode" => self.mode = Mode::parse(s).or(self.mode),
            "color" => self.color = parse_color(s).or(self.color),
            _ => {}
        }
    }

    fn over(&self, base: &Partial) -> Partial {
        Partial {
            path: self.path.clone().or_else(|| base.path.clone()),
            mode: self.mode.or(base.mode),
            color: self.color.or(base.color),
        }
    }
}

/// What one output shows, every key resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// Already `~`-expanded. `None` is the colour alone.
    pub path: Option<PathBuf>,
    pub mode: Mode,
    pub color: Color,
}

/// The whole node: the defaults, and each output's overrides by connector.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Wallpaper {
    base: Partial,
    outputs: HashMap<String, Partial>,
}

impl Wallpaper {
    pub fn for_output(&self, name: &str) -> Spec {
        let p = match self.outputs.get(name) {
            Some(o) => o.over(&self.base),
            None => self.base.clone(),
        };
        Spec {
            path: p.path.filter(|s| !s.trim().is_empty()).map(|s| expand(&s)),
            mode: p.mode.unwrap_or_default(),
            color: p.color.unwrap_or(ec_ui::tokens::color::BASE),
        }
    }
}

/// The `wallpaper` node out of an unfiltered `get_config` reply (no `path`
/// argument — `collections` only rides on that one):
///
/// - `keys[]` rows `{"path": "wallpaper.path"|"wallpaper.mode"|
///   "wallpaper.color", "value": <string>|null}`;
/// - `collections."wallpaper.output"`, one
///   `{"output": "DP-1", "path": .., "mode": .., "color": ..}` per override,
///   a `null` field meaning "use the global value".
///
/// Anything absent, `null` or malformed keeps its default, so an abyss that
/// has never heard of the node — or no abyss at all (`Value::Null`) — is the
/// plain base colour. abyss neither expands nor checks paths; that is
/// [`Wallpaper::for_output`] and the daemon's job.
pub fn from_reply(reply: &Value) -> Wallpaper {
    let mut w = Wallpaper::default();
    for key in reply.get("keys").and_then(Value::as_array).into_iter().flatten() {
        let field = key
            .get("path")
            .and_then(Value::as_str)
            .and_then(|p| p.strip_prefix("wallpaper."));
        if let (Some(field), Some(value)) = (field, key.get("value")) {
            w.base.set(field, value);
        }
    }
    let overrides = reply
        .get("collections")
        .and_then(|c| c.get("wallpaper.output"))
        .and_then(Value::as_array);
    for o in overrides.into_iter().flatten() {
        let Some(name) = o.get("output").and_then(Value::as_str) else {
            continue;
        };
        w.outputs.insert(name.to_owned(), Partial::read(o));
    }
    w
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`, the `#` optional. Alpha is dropped: the
/// background layer has nothing under it to blend with.
pub fn parse_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let nibble = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|n| n * 17);
    let (r, g, b) = match hex.len() {
        3 => (nibble(0)?, nibble(1)?, nibble(2)?),
        6 | 8 => (byte(0)?, byte(2)?, byte(4)?),
        _ => return None,
    };
    Some(Color::from_rgb8(r, g, b))
}

/// A leading `~` is `$HOME`. Nothing else is touched.
pub fn expand(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home,
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: Color = ec_ui::tokens::color::BASE;

    #[test]
    fn no_reply_is_the_base_colour_alone() {
        let w = from_reply(&Value::Null);
        assert_eq!(
            w.for_output("DP-1"),
            Spec {
                path: None,
                mode: Mode::Fill,
                color: BASE
            }
        );
    }

    #[test]
    fn global_keys_and_a_per_output_override() {
        let reply = json!({
            "keys": [
                {"path": "wallpaper.path", "value": "/w/a.png"},
                {"path": "wallpaper.mode", "value": "fit"},
                {"path": "wallpaper.color", "value": "#0b0906ff"},
                {"path": "bar.position", "value": "top"},
            ],
            "collections": {
                "widget": [],
                "wallpaper.output": [
                    {"output": "DP-1", "path": null, "mode": "center", "color": "#223344ff"},
                ],
            },
        });
        let w = from_reply(&reply);
        let other = w.for_output("HDMI-A-1");
        assert_eq!(other.path, Some(PathBuf::from("/w/a.png")));
        assert_eq!(other.mode, Mode::Fit);
        assert_eq!(other.color, BASE);
        let dp = w.for_output("DP-1");
        assert_eq!(
            dp.path,
            Some(PathBuf::from("/w/a.png")),
            "null is the global path"
        );
        assert_eq!(dp.mode, Mode::Center);
        assert_eq!(dp.color, Color::from_rgb8(0x22, 0x33, 0x44));
    }

    #[test]
    fn nulls_and_bad_values_keep_defaults() {
        let reply = json!({
            "keys": [
                {"path": "wallpaper.path", "value": null},
                {"path": "wallpaper.mode", "value": "stretch"},
                {"path": "wallpaper.color", "value": null},
            ],
            "collections": {"wallpaper.output": [
                {"output": "DP-2", "path": "", "mode": null, "color": null},
                {"output": "DP-3", "path": null, "mode": "fit", "color": 7},
                {"path": "/orphan.png"},
            ]},
        });
        let w = from_reply(&reply);
        let base = w.for_output("eDP-1");
        assert_eq!(
            base,
            Spec {
                path: None,
                mode: Mode::Fill,
                color: BASE
            }
        );
        assert_eq!(w.for_output("DP-2").path, None, "an empty path is no picture");
        let dp3 = w.for_output("DP-3");
        assert_eq!(dp3.mode, Mode::Fit);
        assert_eq!(dp3.color, BASE, "a non-string colour is ignored");
    }

    #[test]
    fn colours() {
        assert_eq!(parse_color("#0b0906"), Some(BASE));
        assert_eq!(parse_color("0b0906ff"), Some(BASE));
        assert_eq!(parse_color("#fff"), Some(Color::from_rgb8(255, 255, 255)));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gggggg"), None);
        assert_eq!(parse_color("#ééé"), None);
    }

    #[test]
    fn tilde() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        assert_eq!(expand("~/Pictures/x.png"), home.join("Pictures/x.png"));
        assert_eq!(expand("~"), home);
        assert_eq!(expand("~other/x"), PathBuf::from("~other/x"));
        assert_eq!(expand("/abs/x.png"), PathBuf::from("/abs/x.png"));
    }
}
