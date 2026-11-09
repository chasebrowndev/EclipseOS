// SPDX-License-Identifier: AGPL-3.0-only
//! The icons the Taskbar pages draw, found the way the bar finds them: the
//! symbolic theme for the bar's own marks, an app's `IconThemePath` then the
//! theme for a tray item, its pixmap last. Lookups touch the filesystem, so
//! they run once (the glyphs, at start) or off the UI thread (the tray feed),
//! never from a `view`.
//!
//! The bar (`ec-hyperion-bar/src/icons.rs`) has the same lookups; this
//! copy exists so the preview never links the bar.

use std::path::{Path, PathBuf};

use ec_ui::widget::Art;
use iced::widget::image;

/// Pixel size asked of the theme; SVGs are drawn at the mark's own size.
const LOOKUP_SIZE: u16 = 24;
const THEME: &str = "hicolor";
const SYMBOLIC_THEME: &str = "Adwaita";

/// A named symbolic icon: `<name>-symbolic` in the symbolic theme, then the
/// plain theme, then the bare name.
pub fn symbolic(name: &str) -> Option<PathBuf> {
    let symbolic = format!("{name}-symbolic");
    freedesktop_icons::lookup(&symbolic)
        .with_size(LOOKUP_SIZE)
        .with_theme(SYMBOLIC_THEME)
        .with_cache()
        .find()
        .or_else(|| lookup(&symbolic))
        .or_else(|| lookup(name))
}

fn lookup(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.is_absolute() {
        return direct.exists().then(|| direct.to_path_buf());
    }
    freedesktop_icons::lookup(name)
        .with_size(LOOKUP_SIZE)
        .with_theme(THEME)
        .with_cache()
        .find()
}

/// A tray item's icon. A themed name wins over pixels, as the
/// StatusNotifierItem spec asks; the pixmap is the fallback, silhouetted to
/// a white shape with its own alpha so a vendor logo takes the pane's ink.
pub fn tray(name: &str, theme_path: &str, pixmap: Option<(u32, u32, &[u8])>) -> Art {
    let themed = (!name.is_empty())
        .then(|| {
            (!theme_path.is_empty())
                .then(|| {
                    ["svg", "png"]
                        .iter()
                        .map(|ext| Path::new(theme_path).join(format!("{name}.{ext}")))
                        .find(|p| p.exists())
                })
                .flatten()
                .or_else(|| lookup(name))
        })
        .flatten();
    match (themed, pixmap) {
        (Some(path), _) if !is_raster(&path) => Art::Svg(path),
        (_, Some((w, h, rgba))) if w > 0 && h > 0 => {
            let white: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|px| [u8::MAX, u8::MAX, u8::MAX, px[3]])
                .collect();
            Art::Image(image::Handle::from_rgba(w, h, white))
        }
        (Some(path), _) => Art::Image(image::Handle::from_path(path)),
        (None, _) => Art::None,
    }
}

fn is_raster(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png") || e.eq_ignore_ascii_case("xpm"))
}

/// The bar's own symbolic marks, resolved once.
#[derive(Debug, Clone, Default)]
pub struct Glyphs {
    pub volume: Option<PathBuf>,
    pub network: Option<PathBuf>,
    pub bluetooth: Option<PathBuf>,
    /// The notification history bell `mode "de"` puts before the clock.
    pub bell: Option<PathBuf>,
}

impl Glyphs {
    /// The names the bar asks for (`ec-hyperion-bar`'s radio and status
    /// cells at their resting reading).
    pub fn resolve() -> Glyphs {
        Glyphs {
            volume: symbolic("audio-volume-high"),
            network: symbolic("network-wireless-signal-excellent"),
            bluetooth: symbolic("bluetooth-active"),
            bell: symbolic("preferences-system-notifications"),
        }
    }
}
