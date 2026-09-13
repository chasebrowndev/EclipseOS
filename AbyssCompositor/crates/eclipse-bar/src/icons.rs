// SPDX-License-Identifier: AGPL-3.0-only
//! `app_id` → an icon file on disk.
//!
//! The bar draws real application icons, which means a freedesktop icon-theme
//! lookup, which means touching the filesystem. That is a directory walk, so
//! it happens **once per `app_id`** and is remembered: [`Icons::resolve`] is
//! called when a snapshot arrives, never from the view, and a miss is cached
//! as a miss so that an application with no icon is not re-searched every
//! time the compositor says something.
//!
//! A `secret` window never reaches this module. Its icon identifies it every
//! bit as precisely as its title does, and its title is already withheld
//! (`Window::label`), so the icon is withheld with it — see
//! [`Icons::for_window`], which is the only entry point the view uses.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::{Trust, Window};

/// What the bar found for one `app_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Icon {
    /// A scalable icon: rendered through `iced::widget::svg`.
    Svg(PathBuf),
    /// A raster icon: rendered through `iced::widget::image`.
    Raster(PathBuf),
    /// Nothing was found, or the window is not allowed one. The view draws
    /// the token placeholder; it never leaves a hole.
    Placeholder,
}

impl Icon {
    fn from_path(path: PathBuf) -> Icon {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz") => {
                Icon::Svg(path)
            }
            Some(_) => Icon::Raster(path),
            // No extension at all is not something an icon theme produces;
            // treat it as a miss rather than guessing a decoder.
            None => Icon::Placeholder,
        }
    }
}

/// The theme the lookup starts from. Inheritance (`Inherits=` in
/// `index.theme`, and hicolor as the terminal fallback) is the icon crate's
/// job, so this is a starting point and not a whitelist.
const THEME: &str = "hicolor";

/// The theme the *symbolic* lookup starts from. See [`symbolic`].
const SYMBOLIC_THEME: &str = "Adwaita";

/// The nominal size asked of the icon theme. Larger than [`size::ICON`] on
/// purpose: a 24px raster downscaled to 20 is sharper than a 16px one blown
/// up, and for SVG the request only picks which directory wins.
const LOOKUP_SIZE: u16 = 24;

/// The `app_id` → icon cache.
#[derive(Debug, Default)]
pub struct Icons {
    cache: HashMap<String, Icon>,
}

impl Icons {
    pub fn new() -> Self {
        Icons::default()
    }

    /// Number of `app_id`s resolved so far. Test affordance — it is how
    /// "looked up once, not once per frame" is asserted.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// What to draw for this window.
    ///
    /// The policy gate: a `secret` window gets the placeholder and no
    /// filesystem lookup at all, so there is not even a cache entry from
    /// which its identity could be read back.
    pub fn for_window(&self, w: &Window) -> Icon {
        if w.trust == Trust::Secret {
            return Icon::Placeholder;
        }
        self.cache.get(&w.app_id).cloned().unwrap_or(Icon::Placeholder)
    }

    /// Make sure every window in `windows` has a cache entry. Cheap after the
    /// first pass: the common case is a `HashMap` hit and nothing else.
    pub fn warm(&mut self, windows: &[Window]) {
        for w in windows {
            if w.trust == Trust::Secret || w.app_id.is_empty() {
                continue;
            }
            if !self.cache.contains_key(&w.app_id) {
                let icon = resolve(&w.app_id);
                self.cache.insert(w.app_id.clone(), icon);
            }
        }
    }
}

/// The lookup itself, in fallback order.
///
/// 1. the `app_id` as an icon name,
/// 2. its lowercase form (`Alacritty` ships `alacritty.svg`),
/// 3. its last dotted component (`org.x.Thing` → `Thing`, then `thing`),
/// 4. the `Icon=` key of the matching desktop entry, looked up as a name,
/// 5. the placeholder.
pub fn resolve(app_id: &str) -> Icon {
    let lower = app_id.to_lowercase();
    let tail = app_id.rsplit('.').next().unwrap_or(app_id).to_owned();
    let tail_lower = tail.to_lowercase();

    let mut names: Vec<String> = vec![app_id.to_owned(), lower, tail, tail_lower];
    if let Some(key) = desktop_icon_key(app_id) {
        names.push(key);
    }
    names.dedup();

    for name in names {
        if name.is_empty() {
            continue;
        }
        if let Some(path) = lookup(&name) {
            return Icon::from_path(path);
        }
    }
    Icon::Placeholder
}

/// A named **symbolic** icon from the system icon theme, for the desktop's
/// own furniture — the verbs on the taskbar's context menu, not applications.
///
/// Symbolic and not full-colour on purpose: a symbolic icon is a single-path
/// monochrome glyph, which is the only kind that can be recoloured to the
/// Eclipse palette (`parts::glyph` tints it) instead of dragging some other
/// theme's blues and greens onto a black menu. The freedesktop convention is
/// the `-symbolic` suffix, and `Adwaita` is asked first because it is the set
/// that is guaranteed complete for the standard action names; a plain
/// [`lookup`] is the fallback for a host whose theme spells them differently.
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

/// An `Icon=` value may itself be an absolute path rather than a theme name;
/// the spec allows both, and a surprising number of entries use the path.
fn lookup(name: &str) -> Option<PathBuf> {
    let direct = std::path::Path::new(name);
    if direct.is_absolute() {
        return direct.exists().then(|| direct.to_path_buf());
    }
    freedesktop_icons::lookup(name)
        .with_size(LOOKUP_SIZE)
        .with_theme(THEME)
        .with_cache()
        .find()
}

/// The `Icon=` key of `<app_id>.desktop`, if such a file exists.
///
/// `eclipse_services::apps::Entry` does not carry `Icon` — the launcher has
/// never needed it — so rather than duplicate the scanner this reuses its
/// `search_path()` and reads the one key out of the one file. That is a
/// single `read_to_string` per *missing* app_id, cached like the rest.
fn desktop_icon_key(app_id: &str) -> Option<String> {
    let file = format!("{app_id}.desktop");
    for dir in eclipse_services::apps::search_path() {
        let path = dir.join(&file);
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(value) = icon_key(&body) {
            return Some(value);
        }
    }
    None
}

/// Pull `Icon=` out of the `[Desktop Entry]` group of a desktop file.
///
/// Hand-rolled for the same reason `apps.rs` hand-rolls its parser: the file
/// format is four lines of INI and a dependency to read it is a dependency to
/// audit. Only the first group is considered — a `[Desktop Action ...]` group
/// has its own `Icon=` and it is not the application's.
pub(crate) fn icon_key(body: &str) -> Option<String> {
    let mut in_entry = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(value) = line.strip_prefix("Icon=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(app_id: &str, trust: Trust) -> Window {
        Window {
            handle: 1,
            app_id: app_id.to_owned(),
            title: "t".into(),
            workspace: Some(1),
            focused: false,
            minimized: false,
            pid: None,
            trust,
        }
    }

    /// The policy invariant. An icon names the application as precisely as a
    /// title names the document, so a `secret` window gets neither.
    #[test]
    fn a_secret_window_never_gets_its_application_icon() {
        let mut icons = Icons::new();
        let secret = window("firefox", Trust::Secret);
        icons.warm(std::slice::from_ref(&secret));
        assert_eq!(icons.for_window(&secret), Icon::Placeholder);
        // …and nothing about it was even written down.
        assert!(icons.is_empty());
    }

    /// A private window with the same app_id resolves normally, so the test
    /// above is about trust and not about `firefox` being unfindable.
    #[test]
    fn trust_is_what_withholds_the_icon_not_the_lookup() {
        let mut icons = Icons::new();
        let private = window("firefox", Trust::Private);
        icons.warm(std::slice::from_ref(&private));
        // One entry either way: found or cached as a miss. What matters is
        // that the private window was looked up at all.
        assert_eq!(icons.len(), 1);
    }

    #[test]
    fn an_app_id_is_looked_up_once_and_then_remembered() {
        let mut icons = Icons::new();
        let wins = vec![window("kitty", Trust::Private), window("kitty", Trust::Private)];
        icons.warm(&wins);
        assert_eq!(icons.len(), 1);
        icons.warm(&wins);
        assert_eq!(icons.len(), 1);
    }

    #[test]
    fn a_window_with_no_app_id_is_not_cached_under_the_empty_string() {
        let mut icons = Icons::new();
        icons.warm(&[window("", Trust::Private)]);
        assert!(icons.is_empty());
    }

    #[test]
    fn an_unfindable_app_id_degrades_to_the_placeholder() {
        assert_eq!(resolve("org.example.NoSuchApplication.ZZZ"), Icon::Placeholder);
    }

    #[test]
    fn the_icon_key_comes_from_the_desktop_entry_group_only() {
        let body = "[Desktop Entry]\nName=Thing\nIcon=thing\n\n[Desktop Action New]\nIcon=thing-new\n";
        assert_eq!(icon_key(body).as_deref(), Some("thing"));
        assert_eq!(icon_key("[Desktop Action New]\nIcon=thing-new\n"), None);
        assert_eq!(icon_key("[Desktop Entry]\nIcon=\n"), None);
    }

    #[test]
    fn the_extension_picks_the_renderer() {
        assert!(matches!(Icon::from_path(PathBuf::from("/a/b.svg")), Icon::Svg(_)));
        assert!(matches!(
            Icon::from_path(PathBuf::from("/a/b.PNG")),
            Icon::Raster(_)
        ));
        assert_eq!(Icon::from_path(PathBuf::from("/a/b")), Icon::Placeholder);
    }
}
