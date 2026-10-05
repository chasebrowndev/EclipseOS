// SPDX-License-Identifier: AGPL-3.0-only
//! The transition-shader catalog (ADR 0073 "Transition shaders"; ADR 0066 hook
//! `transition-shaders`).
//!
//! An animation pack ships, per style, `<pack>/<style>.kdl` and the fragment
//! shader it names, as siblings under [`TRANSITIONS_DIR`]:
//!
//! ```kdl
//! label "Embers dissolve"
//! events "window-close" "minimize"
//! shader "embers.frag"
//! margin 24
//! duration-ms 420
//! curve "ease-out"
//! ```
//!
//! A pack may also ship one-click presets, `<pack>/presets/<name>.kdl`:
//!
//! ```kdl
//! label "Embers"
//! base "smooth"
//! style "window-open" "embers"
//! ```
//!
//! The id is `pack:name`; `base` is a built-in preset (not `off`) and each
//! `style` names an event and a bare style of the same pack that serves it.
//! A refused preset drops only itself. The `presets/` subdirectory is never
//! scanned as styles.
//!
//! The style id is `pack:style`. `ec-abyss` reads the catalog only while the
//! hook is on; reading lives here, drawing in `ec-abyss-render`.
//!
//! The directory is package-owned, like the widget catalog's, and the limits
//! are the ones the ADR names: a file is at most [`MAX_FILE_BYTES`], the
//! shader is a plain sibling file (no path, no symlink), pack and style names
//! are `[a-z0-9-]+`, and the shader may use neither `#extension` nor `while`.
//! A style that breaks a rule is skipped with its reason in
//! [`Catalog::rejected`]; it never stops the others, and never stops abyss.

use std::io::Read;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlValue};

use crate::animations::{Curve, Event, Preset};

/// Package-owned and root-writable. The one place the path is named: the
/// loader takes the root as a parameter, so only `ec-abyss` passes this.
pub const TRANSITIONS_DIR: &str = "/usr/share/eclipse/transitions";

/// Largest `.kdl` or `.frag` file read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Largest `margin`, logical px.
pub const MAX_MARGIN: i64 = 256;
/// Longest `duration-ms`.
pub const MAX_DURATION_MS: i64 = 2000;
/// Longest `label`, in chars.
pub const MAX_LABEL: usize = 64;

/// The events the compositor draws a shader for. A style may declare others
/// (a pack written for a newer abyss); they load but are not offered.
pub const DRAWN_EVENTS: [Event; 3] = [Event::WindowOpen, Event::WindowClose, Event::Minimize];

/// One validated style.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionStyle {
    /// `pack:style`.
    pub id: String,
    pub label: String,
    pub events: Vec<Event>,
    /// The fragment shader body, already checked by [`check_shader`].
    pub source: String,
    /// Logical px the quad is grown by on every side.
    pub margin: u32,
    pub duration_ms: u32,
    pub curve: Curve,
}

impl TransitionStyle {
    /// Whether the style declares `ev` and the compositor can draw it.
    pub fn serves(&self, ev: Event) -> bool {
        DRAWN_EVENTS.contains(&ev) && self.events.contains(&ev)
    }
}

/// A style that was refused, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub path: PathBuf,
    pub reason: String,
}

/// One validated pack preset: a built-in `base` with some events pointed at
/// the pack's styles.
#[derive(Debug, Clone, PartialEq)]
pub struct PackPreset {
    /// `pack:name`.
    pub id: String,
    pub label: String,
    pub base: Preset,
    /// Event and full style id (`pack:style`).
    pub styles: Vec<(Event, String)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub styles: Vec<TransitionStyle>,
    pub presets: Vec<PackPreset>,
    pub rejected: Vec<Rejected>,
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&TransitionStyle> {
        self.styles.iter().find(|s| s.id == id)
    }
}

/// `[a-z0-9-]+`: a pack or style name.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The limits a shader body must meet before it is ever handed to the driver:
/// no `#extension` (the header owns the extensions) and no `while` (no
/// unbounded loops; GLSL ES 1.00 `for` needs constant bounds).
pub fn check_shader(src: &str) -> Result<(), String> {
    if src.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("shader is over {MAX_FILE_BYTES} bytes"));
    }
    if src.contains('\0') {
        return Err("shader contains a NUL byte".into());
    }
    for line in src.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('#') {
            let rest = rest.trim_start();
            if rest.starts_with("extension") {
                return Err("`#extension` is not allowed".into());
            }
            if rest.starts_with("version") {
                return Err("`#version` is not allowed; the header sets it".into());
            }
        }
    }
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = src.as_bytes();
    let mut from = 0;
    while let Some(at) = src[from..].find("while") {
        let start = from + at;
        let end = start + "while".len();
        let before = start == 0 || !ident(bytes[start - 1]);
        let after = end >= bytes.len() || !ident(bytes[end]);
        if before && after {
            return Err("`while` is not allowed".into());
        }
        from = end;
    }
    Ok(())
}

/// A shader file name is one plain component ending in `.frag`: no directory,
/// no `..`, no dot-file. The file it names must sit next to the `.kdl`.
fn sibling_name(name: &str) -> Result<&str, String> {
    let mut parts = Path::new(name).components();
    let plain = matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !plain || name.contains(['/', '\\', '\0']) || name.starts_with('.') || !name.ends_with(".frag") {
        return Err(format!("shader `{name}` must be a sibling `*.frag` file"));
    }
    Ok(name)
}

/// Read a regular, non-symlink file of at most [`MAX_FILE_BYTES`] as UTF-8.
fn read_limited(path: &Path) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{} is over {MAX_FILE_BYTES} bytes", path.display()));
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_string(&mut text))
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{} is over {MAX_FILE_BYTES} bytes", path.display()));
    }
    Ok(text)
}

/// Parse one style file. `shader_of` reads the named sibling.
fn parse_style(
    id: String,
    kdl: &str,
    shader_of: impl FnOnce(&str) -> Result<String, String>,
) -> Result<TransitionStyle, String> {
    let doc: KdlDocument = kdl.parse().map_err(|e| format!("not KDL: {e}"))?;
    let mut label = None;
    let mut events: Vec<Event> = Vec::new();
    let mut shader = None;
    let mut margin = 0i64;
    let mut duration_ms = 250i64;
    let mut curve = Curve::EaseOut;
    for node in doc.nodes() {
        let key = node.name().value();
        let args: Vec<&KdlValue> = node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .map(|e| e.value())
            .collect();
        if node.entries().iter().any(|e| e.name().is_some()) || node.children().is_some() {
            return Err(format!("`{key}` takes only plain arguments"));
        }
        let one = || match args.as_slice() {
            [v] => Ok(*v),
            _ => Err(format!("`{key}` takes one value")),
        };
        let int = |lo: i64, hi: i64| {
            one()?
                .as_integer()
                .and_then(|n| i64::try_from(n).ok())
                .filter(|n| (lo..=hi).contains(n))
                .ok_or_else(|| format!("`{key}` must be an integer in {lo}..={hi}"))
        };
        let text = || {
            one()?
                .as_string()
                .map(str::to_owned)
                .ok_or_else(|| format!("`{key}` must be a string"))
        };
        match key {
            "label" => {
                let l = text()?;
                if l.is_empty() || l.chars().count() > MAX_LABEL || l.chars().any(char::is_control) {
                    return Err(format!("`label` must be 1..={MAX_LABEL} printable chars"));
                }
                label = Some(l);
            }
            "events" => {
                if args.is_empty() {
                    return Err("`events` names at least one event".into());
                }
                for v in &args {
                    let s = v.as_string().ok_or("`events` takes strings")?;
                    let ev = Event::parse(s).ok_or_else(|| format!("unknown event `{s}`"))?;
                    if !events.contains(&ev) {
                        events.push(ev);
                    }
                }
            }
            "shader" => shader = Some(text()?),
            "margin" => margin = int(0, MAX_MARGIN)?,
            "duration-ms" => duration_ms = int(1, MAX_DURATION_MS)?,
            "curve" => {
                let c = text()?;
                curve = Curve::parse(&c).ok_or_else(|| format!("unknown curve `{c}`"))?;
            }
            other => return Err(format!("unknown key `{other}`")),
        }
    }
    let label = label.ok_or("missing `label`")?;
    if events.is_empty() {
        return Err("missing `events`".into());
    }
    let shader = shader.ok_or("missing `shader`")?;
    let source = shader_of(sibling_name(&shader)?)?;
    check_shader(&source)?;
    Ok(TransitionStyle {
        id,
        label,
        events,
        source,
        margin: margin as u32,
        duration_ms: duration_ms as u32,
        curve,
    })
}

/// Parse one preset file of `pack`; styles are checked against `styles`.
fn parse_preset(pack: &str, id: String, kdl: &str, styles: &[TransitionStyle]) -> Result<PackPreset, String> {
    let doc: KdlDocument = kdl.parse().map_err(|e| format!("not KDL: {e}"))?;
    let mut label = None;
    let mut base = None;
    let mut picks: Vec<(Event, String)> = Vec::new();
    for node in doc.nodes() {
        let key = node.name().value();
        if node.entries().iter().any(|e| e.name().is_some()) || node.children().is_some() {
            return Err(format!("`{key}` takes only plain arguments"));
        }
        let args: Vec<&str> = node
            .entries()
            .iter()
            .map(|e| {
                e.value()
                    .as_string()
                    .ok_or_else(|| format!("`{key}` takes strings"))
            })
            .collect::<Result<_, _>>()?;
        match (key, args.as_slice()) {
            ("label", [l]) => {
                if l.is_empty() || l.chars().count() > MAX_LABEL || l.chars().any(char::is_control) {
                    return Err(format!("`label` must be 1..={MAX_LABEL} printable chars"));
                }
                label = Some((*l).to_owned());
            }
            ("base", [b]) => {
                base = Some(
                    Preset::parse(b)
                        .filter(|p| *p != Preset::Off)
                        .ok_or_else(|| format!("unknown base `{b}`"))?,
                );
            }
            ("style", [ev, name]) => {
                let event = Event::parse(ev).ok_or_else(|| format!("unknown event `{ev}`"))?;
                if picks.iter().any(|(e, _)| *e == event) {
                    return Err(format!("event `{ev}` is given twice"));
                }
                if !valid_name(name) {
                    return Err(format!("style `{name}` must be a bare [a-z0-9-]+ name"));
                }
                let full = format!("{pack}:{name}");
                match styles.iter().find(|s| s.id == full) {
                    Some(s) if s.serves(event) => picks.push((event, full)),
                    Some(_) => return Err(format!("`{full}` does not serve `{ev}`")),
                    None => return Err(format!("`{full}` is not in the pack")),
                }
            }
            ("label" | "base" | "style", _) => return Err(format!("`{key}` has the wrong arguments")),
            (other, _) => return Err(format!("unknown key `{other}`")),
        }
    }
    Ok(PackPreset {
        id,
        label: label.ok_or("missing `label`")?,
        base: base.ok_or("missing `base`")?,
        styles: picks,
    })
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Read the catalog under `root`. A missing root is an empty catalog; every
/// refusal is recorded and logged once, and the rest still load. The first
/// style with a given id wins.
pub fn load_from(root: &Path) -> Catalog {
    let mut cat = Catalog::default();
    let reject = |cat: &mut Catalog, path: PathBuf, reason: String| {
        tracing::warn!(path = %path.display(), %reason, "transition style refused");
        cat.rejected.push(Rejected { path, reason });
    };
    for pack_dir in sorted_entries(root) {
        let Some(pack) = pack_dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        // A pack is a real directory; a link could point outside the package.
        let is_dir = std::fs::symlink_metadata(&pack_dir).is_ok_and(|m| m.is_dir());
        if !is_dir {
            continue;
        }
        if !valid_name(&pack) {
            reject(&mut cat, pack_dir, "pack name must be [a-z0-9-]+".into());
            continue;
        }
        for file in sorted_entries(&pack_dir) {
            if file.extension().is_none_or(|x| x != "kdl") {
                continue;
            }
            let Some(style) = file.file_stem().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            if !valid_name(&style) {
                reject(&mut cat, file, "style name must be [a-z0-9-]+".into());
                continue;
            }
            let id = format!("{pack}:{style}");
            if cat.get(&id).is_some() {
                reject(&mut cat, file, format!("`{id}` is given twice"));
                continue;
            }
            let parsed = read_limited(&file)
                .and_then(|text| parse_style(id, &text, |name| read_limited(&pack_dir.join(name))));
            match parsed {
                Ok(s) => cat.styles.push(s),
                Err(reason) => reject(&mut cat, file, reason),
            }
        }
        // Presets read after the pack's styles, so they can name them.
        let pdir = pack_dir.join("presets");
        if !std::fs::symlink_metadata(&pdir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        for file in sorted_entries(&pdir) {
            if file.extension().is_none_or(|x| x != "kdl") {
                continue;
            }
            let Some(name) = file.file_stem().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            if !valid_name(&name) {
                reject(&mut cat, file, "preset name must be [a-z0-9-]+".into());
                continue;
            }
            let id = format!("{pack}:{name}");
            if cat.presets.iter().any(|p| p.id == id) {
                reject(&mut cat, file, format!("`{id}` is given twice"));
                continue;
            }
            let parsed = read_limited(&file).and_then(|text| parse_preset(&pack, id, &text, &cat.styles));
            match parsed {
                Ok(p) => cat.presets.push(p),
                Err(reason) => reject(&mut cat, file, reason),
            }
        }
    }
    cat
}

#[cfg(test)]
mod tests {
    use super::*;

    const KDL: &str = "label \"Swirl\"\nevents \"window-open\" \"window-close\"\nshader \"swirl.frag\"\n\
                       margin 16\nduration-ms 300\ncurve \"ease-in-out\"\n";
    const FRAG: &str = "void main() { gl_FragColor = texture2D(tex, v_coords) * alpha; }\n";

    fn root(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("abyss-transitions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (name, text) in files {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d
    }

    #[test]
    fn a_pack_loads_as_pack_colon_style() {
        let d = root("ok", &[("fx/swirl.kdl", KDL), ("fx/swirl.frag", FRAG)]);
        let cat = load_from(&d);
        assert!(cat.rejected.is_empty(), "{:?}", cat.rejected);
        let s = cat.get("fx:swirl").expect("loaded");
        assert_eq!(s.label, "Swirl");
        assert_eq!((s.margin, s.duration_ms, s.curve), (16, 300, Curve::EaseInOut));
        assert!(s.serves(Event::WindowOpen) && !s.serves(Event::Minimize));
        let _ = std::fs::remove_dir_all(d);
    }

    /// The eclipseos-anim-pack styles in `packaging/transitions/` pass the same
    /// checks abyss runs on the installed copy.
    #[test]
    fn the_shipped_anim_pack_loads() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packaging/transitions");
        let cat = load_from(&d);
        assert!(cat.rejected.is_empty(), "{:?}", cat.rejected);
        let mut ids: Vec<&str> = cat.styles.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [
                "anim-pack:embers",
                "anim-pack:genie",
                "anim-pack:materialize",
                "anim-pack:shatter"
            ]
        );
        assert!(cat.styles.iter().all(|s| s.events.iter().all(|&e| s.serves(e))));
        let p = cat
            .presets
            .iter()
            .find(|p| p.id == "anim-pack:embers")
            .expect("preset");
        assert_eq!((p.label.as_str(), p.base), ("Ultra", Preset::Smooth));
        assert_eq!(p.styles.len(), 3);
        assert!(p
            .styles
            .contains(&(Event::Minimize, "anim-pack:genie".to_owned())));
    }

    fn preset_root(tag: &str, preset: &str) -> PathBuf {
        root(
            tag,
            &[
                ("fx/swirl.kdl", KDL),
                ("fx/swirl.frag", FRAG),
                ("fx/presets/p.kdl", preset),
            ],
        )
    }

    #[test]
    fn a_good_preset_loads_and_presets_are_not_styles() {
        let d = preset_root(
            "pgood",
            "label \"P\"\nbase \"lively\"\nstyle \"window-open\" \"swirl\"\nstyle \"window-close\" \"swirl\"\n",
        );
        let cat = load_from(&d);
        assert!(cat.rejected.is_empty(), "{:?}", cat.rejected);
        assert_eq!(cat.styles.len(), 1);
        assert_eq!(cat.presets.len(), 1);
        let p = &cat.presets[0];
        assert_eq!((p.id.as_str(), p.base), ("fx:p", Preset::Lively));
        assert_eq!(p.styles[1], (Event::WindowClose, "fx:swirl".to_owned()));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn bad_presets_are_dropped_alone() {
        let head = "label \"P\"\nbase \"smooth\"\n";
        for (tag, bad) in [
            ("pev", format!("{head}style \"windows\" \"swirl\"\n")),
            ("pbase", "label \"P\"\nbase \"huge\"\n".to_owned()),
            ("poff", "label \"P\"\nbase \"off\"\n".to_owned()),
            (
                "pdup",
                format!("{head}style \"window-open\" \"swirl\"\nstyle \"window-open\" \"swirl\"\n"),
            ),
            ("pmissing", format!("{head}style \"window-open\" \"nope\"\n")),
            ("pserve", format!("{head}style \"minimize\" \"swirl\"\n")),
            ("pcross", format!("{head}style \"window-open\" \"other:swirl\"\n")),
            ("pkey", format!("{head}surprise 1\n")),
        ] {
            let d = preset_root(tag, &bad);
            let cat = load_from(&d);
            assert!(cat.presets.is_empty(), "{tag}");
            assert_eq!(cat.rejected.len(), 1, "{tag}: {:?}", cat.rejected);
            assert!(cat.get("fx:swirl").is_some(), "{tag}");
            let _ = std::fs::remove_dir_all(d);
        }
        let big = format!("label \"P\"\nbase \"smooth\"\n// {}", "x".repeat(70_000));
        let d = preset_root("pbig", &big);
        let cat = load_from(&d);
        assert!(cat.presets.is_empty() && cat.rejected.len() == 1);
        let _ = std::fs::remove_dir_all(d);
        let d = preset_root("plink", "");
        std::fs::write(d.join("real.txt"), "label \"P\"\nbase \"smooth\"\n").unwrap();
        std::fs::remove_file(d.join("fx/presets/p.kdl")).unwrap();
        std::os::unix::fs::symlink(d.join("real.txt"), d.join("fx/presets/p.kdl")).unwrap();
        assert!(load_from(&d).presets.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_missing_root_is_an_empty_catalog() {
        let d = std::env::temp_dir().join(format!("abyss-transitions-absent-{}", std::process::id()));
        assert_eq!(load_from(&d), Catalog::default());
    }

    #[test]
    fn while_and_extension_are_rejected() {
        for (tag, bad) in [
            ("while", "void main() { int i = 0; while (i < 3) { i++; } }"),
            (
                "ext",
                "#extension GL_OES_standard_derivatives : enable\nvoid main() {}",
            ),
            ("ext-space", "  #  extension GL_X : enable\nvoid main() {}"),
        ] {
            let d = root(
                tag,
                &[
                    ("fx/a.kdl", KDL.replace("swirl", "a").as_str()),
                    ("fx/a.frag", bad),
                ],
            );
            let cat = load_from(&d);
            assert!(cat.styles.is_empty(), "{tag}");
            assert_eq!(cat.rejected.len(), 1, "{tag}");
            let _ = std::fs::remove_dir_all(d);
        }
        // `while` inside a longer identifier is fine.
        assert!(check_shader("float meanwhile = 1.0; void main() {}").is_ok());
    }

    #[test]
    fn an_oversized_file_is_rejected() {
        let big = "// x\n".repeat(20_000);
        assert!(big.len() as u64 > MAX_FILE_BYTES);
        let d = root("big", &[("fx/swirl.kdl", KDL), ("fx/swirl.frag", &big)]);
        let cat = load_from(&d);
        assert!(cat.styles.is_empty());
        assert!(cat.rejected[0].reason.contains("over"), "{:?}", cat.rejected);
        let d2 = root(
            "bigkdl",
            &[
                ("fx/swirl.kdl", &format!("{KDL}// {big}")),
                ("fx/swirl.frag", FRAG),
            ],
        );
        assert!(load_from(&d2).styles.is_empty());
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(d2);
    }

    #[test]
    fn names_and_paths_cannot_escape() {
        for bad in [
            "../swirl.frag",
            "/etc/passwd.frag",
            "sub/swirl.frag",
            ".hidden.frag",
            "swirl.glsl",
        ] {
            assert!(sibling_name(bad).is_err(), "{bad}");
        }
        let d = root(
            "names",
            &[
                ("Bad_Pack/a.kdl", KDL),
                ("fx/Up.kdl", KDL),
                ("fx/esc.kdl", &KDL.replace("swirl.frag", "../esc.frag")),
                ("esc.frag", FRAG),
            ],
        );
        let cat = load_from(&d);
        assert!(cat.styles.is_empty());
        assert_eq!(cat.rejected.len(), 3, "{:?}", cat.rejected);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_symlinked_shader_is_refused() {
        let d = root("link", &[("fx/swirl.kdl", KDL), ("outside.frag", FRAG)]);
        std::os::unix::fs::symlink(d.join("outside.frag"), d.join("fx/swirl.frag")).unwrap();
        assert!(load_from(&d).styles.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn bad_keys_and_values_skip_only_that_style() {
        let d = root(
            "mixed",
            &[
                ("fx/good.kdl", &KDL.replace("swirl.frag", "good.frag")),
                ("fx/good.frag", FRAG),
                (
                    "fx/odd.kdl",
                    &format!("{}surprise 1\n", KDL.replace("swirl.frag", "odd.frag")),
                ),
                ("fx/odd.frag", FRAG),
                (
                    "fx/ev.kdl",
                    &KDL.replace("window-open", "windows")
                        .replace("swirl.frag", "ev.frag"),
                ),
                ("fx/ev.frag", FRAG),
            ],
        );
        let cat = load_from(&d);
        assert!(cat.get("fx:good").is_some());
        assert_eq!(cat.styles.len(), 1);
        assert_eq!(cat.rejected.len(), 2);
        let _ = std::fs::remove_dir_all(d);
    }
}
