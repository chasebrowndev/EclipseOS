// SPDX-License-Identifier: AGPL-3.0-only
//! A notification's picture: the `image-data` pixels if it sent any, else its
//! icon (a path, a `file://` URI or a theme name), else the desktop entry's
//! icon. Resolved once when the notification arrives, never from the view:
//! the theme lookup walks the disk.

use std::path::{Path, PathBuf};

use iced::widget::{image, svg};

use ec_services::notifications::Notification;

/// The theme the lookup starts from; the crate follows `Inherits=`.
const THEME: &str = "hicolor";
/// Asked second: it ships the standard action and status icons.
const FALLBACK_THEME: &str = "Adwaita";
/// The size the lookup asks the theme for; the card draws it smaller.
const LOOKUP: u16 = 64;

/// What to draw in the avatar slot.
#[derive(Debug, Clone)]
pub enum Art {
    Raster(image::Handle),
    Svg(svg::Handle),
    /// Nothing resolved: the view draws the app's initial on a glass tile.
    None,
}

/// Resolve `n`'s picture.
pub fn resolve(n: &Notification) -> Art {
    if let Some(img) = &n.image {
        let want = (img.width as usize)
            .checked_mul(img.height as usize)
            .and_then(|px| px.checked_mul(4));
        if img.width > 0 && img.height > 0 && want == Some(img.rgba.len()) {
            return Art::Raster(image::Handle::from_rgba(img.width, img.height, img.rgba.clone()));
        }
    }
    [n.icon.as_str(), n.desktop_entry.as_str()]
        .into_iter()
        .find_map(path_for)
        .map_or(Art::None, |p| from_path(&p))
}

fn from_path(path: &Path) -> Art {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        Art::Svg(svg::Handle::from_path(path))
    } else {
        Art::Raster(image::Handle::from_path(path))
    }
}

fn path_for(name: &str) -> Option<PathBuf> {
    let name = name.strip_prefix("file://").unwrap_or(name);
    if name.is_empty() {
        return None;
    }
    let direct = Path::new(name);
    if direct.is_absolute() {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    [THEME, FALLBACK_THEME].into_iter().find_map(|theme| {
        freedesktop_icons::lookup(name)
            .with_size(LOOKUP)
            .with_theme(theme)
            .with_cache()
            .find()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::notifications::Image;

    #[test]
    fn nothing_to_draw_is_none() {
        assert!(matches!(resolve(&Notification::default()), Art::None));
    }

    #[test]
    fn pixels_win_and_a_torn_buffer_is_ignored() {
        let mut n = Notification {
            image: Some(Image {
                width: 2,
                height: 1,
                rgba: vec![0; 8],
            }),
            ..Notification::default()
        };
        assert!(matches!(resolve(&n), Art::Raster(_)));
        n.image = Some(Image {
            width: 2,
            height: 2,
            rgba: vec![0; 8],
        });
        assert!(matches!(resolve(&n), Art::None));
    }

    #[test]
    fn a_missing_absolute_path_is_none() {
        let n = Notification {
            icon: "/nonexistent/ec-toasts.png".into(),
            ..Notification::default()
        };
        assert!(matches!(resolve(&n), Art::None));
    }
}
