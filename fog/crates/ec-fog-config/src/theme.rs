// SPDX-License-Identifier: AGPL-3.0-only

//! The EclipseOS theme values Fog shares with the compositor (FOG §Visual
//! design, "Theming"): corner radius, blur, window opacity, shadow and the
//! animation switch, read from the `decoration` and `animations` blocks of
//! `abyss.kdl`: first `/etc/eclipse/abyss.kdl`, then
//! `$XDG_CONFIG_HOME/eclipse/abyss.kdl` on top, as the compositor layers them.
//!
//! The compositor owns that file and validates it; this reader is lenient.
//! A missing file, a syntax error or a value of the wrong type leaves the
//! compositor's schema default, so Fog never refuses to start over a
//! compositor setting. Read once, before iced starts.

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode, KdlValue};

/// The compositor-owned theme values, with abyss's schema defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// `decoration.rounding`: window corner radius in logical pixels.
    pub rounding: u32,
    /// `decoration.blur.enabled`.
    pub blur: bool,
    /// `decoration.blur.size`: the dual-Kawase sample offset base.
    pub blur_size: u32,
    /// `decoration.blur.passes`, 1 to 6.
    pub blur_passes: u32,
    /// `decoration.active-opacity`: the compositor blurs behind a toplevel
    /// only when this is below 1.
    pub active_opacity: f32,
    /// `decoration.shadow.enabled`.
    pub shadow: bool,
    /// `decoration.shadow.range`: shadow spread in logical pixels.
    pub shadow_range: u32,
    /// `animations.enabled`.
    pub animations: bool,
}

impl Default for Theme {
    /// abyss's schema defaults (`abyss/crates/ec-abyss-config/src/lib.rs`).
    fn default() -> Theme {
        Theme {
            rounding: 13,
            blur: true,
            blur_size: 8,
            blur_passes: 2,
            active_opacity: 0.87,
            shadow: false,
            shadow_range: 20,
            animations: true,
        }
    }
}

impl Theme {
    /// `text` laid over `self`. Anything unreadable is skipped.
    pub fn overlay(mut self, text: &str) -> Theme {
        let Ok(doc) = text.parse::<KdlDocument>() else {
            return self;
        };
        for node in doc.nodes() {
            match node.name().value() {
                "decoration" => self.decoration(node),
                "animations" => {
                    for n in children(node) {
                        if n.name().value() == "enabled" {
                            set_bool(&mut self.animations, n);
                        }
                    }
                }
                _ => {}
            }
        }
        self
    }

    fn decoration(&mut self, node: &KdlNode) {
        for n in children(node) {
            match n.name().value() {
                "rounding" => set_int(&mut self.rounding, n, 0, 64),
                "active-opacity" => {
                    if let Some(v) = first(n).and_then(number) {
                        self.active_opacity = v.clamp(0.0, 1.0) as f32;
                    }
                }
                "blur" => {
                    for b in children(n) {
                        match b.name().value() {
                            "enabled" => set_bool(&mut self.blur, b),
                            "size" => set_int(&mut self.blur_size, b, 1, 64),
                            "passes" => set_int(&mut self.blur_passes, b, 1, 6),
                            _ => {}
                        }
                    }
                }
                "shadow" => {
                    for s in children(n) {
                        match s.name().value() {
                            "enabled" => set_bool(&mut self.shadow, s),
                            "range" => set_int(&mut self.shadow_range, s, 0, 256),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether the compositor blurs what is behind a Fog window. abyss
    /// blurs a toplevel only when its opacity is below 1.
    pub fn compositor_blurs(&self) -> bool {
        self.blur && self.active_opacity < 1.0
    }
}

/// System file, then the user's, as abyss layers them.
pub fn paths() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/etc/eclipse/abyss.kdl")];
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        });
    if let Some(base) = user {
        v.push(base.join("eclipse").join("abyss.kdl"));
    }
    v
}

/// The theme from `paths`, over the schema defaults.
pub fn load(paths: &[impl AsRef<Path>]) -> Theme {
    paths.iter().fold(Theme::default(), |t, p| {
        match std::fs::read_to_string(p.as_ref()) {
            Ok(text) => t.overlay(&text),
            Err(_) => t,
        }
    })
}

fn children(node: &KdlNode) -> &[KdlNode] {
    node.children().map(|d| d.nodes()).unwrap_or_default()
}

fn first(n: &KdlNode) -> Option<&KdlValue> {
    n.entries()
        .first()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
}

fn number(v: &KdlValue) -> Option<f64> {
    match v {
        KdlValue::Integer(i) => Some(*i as f64),
        KdlValue::Float(f) => Some(*f),
        _ => None,
    }
}

fn set_bool(slot: &mut bool, n: &KdlNode) {
    if let Some(b) = first(n).and_then(KdlValue::as_bool) {
        *slot = b;
    }
}

fn set_int(slot: &mut u32, n: &KdlNode, min: u32, max: u32) {
    if let Some(KdlValue::Integer(i)) = first(n) {
        *slot = (*i).clamp(min as i128, max as i128) as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_file_overrides_schema_defaults() {
        let t = Theme::default().overlay(
            "decoration {\n  blur { enabled #true; passes 3 }\n  rounding 9\n  \
             shadow { enabled #true }\n  active-opacity 0.87\n}\n\
             animations { enabled #false }\n",
        );
        assert_eq!(t.rounding, 9);
        assert!(t.blur && t.shadow && !t.animations);
        assert_eq!((t.blur_size, t.blur_passes, t.shadow_range), (8, 3, 20));
        assert!((t.active_opacity - 0.87).abs() < 1e-6);
        assert!(t.compositor_blurs());
    }

    #[test]
    fn opaque_windows_are_not_blurred() {
        // Stock abyss ships active-opacity 0.87, so a default install blurs.
        let t = Theme::default();
        assert!(t.blur && t.compositor_blurs());
        let t = t.overlay("decoration { active-opacity 1 }");
        assert!(!t.compositor_blurs());
        let t =
            Theme::default().overlay("decoration { active-opacity 0.9; blur { enabled #false } }");
        assert!(!t.compositor_blurs());
    }

    #[test]
    fn junk_is_ignored_and_ranges_clamped() {
        let d = Theme::default();
        assert_eq!(d.overlay("decoration {"), d);
        assert_eq!(
            d.overlay("decoration { rounding \"big\"; blur { size #true } }"),
            d
        );
        let t = d
            .overlay("decoration { rounding 900; blur { passes 40; size 0 }; active-opacity 3.0 }");
        assert_eq!((t.rounding, t.blur_passes, t.blur_size), (64, 6, 1));
        assert_eq!(t.active_opacity, 1.0);
    }

    #[test]
    fn later_files_win_and_missing_files_are_skipped() {
        let dir = std::env::temp_dir().join(format!("fog-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (sys, user) = (dir.join("sys.kdl"), dir.join("user.kdl"));
        std::fs::write(&sys, "decoration { rounding 4; blur { size 12 } }").unwrap();
        std::fs::write(&user, "decoration { rounding 20 }").unwrap();
        let t = load(&[sys.clone(), dir.join("missing.kdl"), user]);
        assert_eq!((t.rounding, t.blur_size), (20, 12));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
