// SPDX-License-Identifier: AGPL-3.0-only
//! KDL configuration (COMP-13 §1).
//!
//! Search path, later files overriding earlier ones:
//!   1. `/etc/eclipse/helios.kdl`
//!   2. `$XDG_CONFIG_HOME/eclipse/helios.kdl`
//!   3. `$XDG_CONFIG_HOME/eclipse/helios.d/*.kdl` (sorted)
//!   4. `$XDG_CONFIG_HOME/helios/config.kdl` (compatibility fallback)
//!      `--config <path>` replaces the whole search path with that one file.
//!
//! Parsing never fails hard: every malformed node is logged and skipped, and a
//! file that will not parse at all leaves the defaults in place.

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode, KdlValue};
use smithay::input::keyboard::{xkb, Keysym, ModifiersState};

use crate::input::{Action, Bind, Direction, Mods};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Dwindle,
    Master,
}

#[derive(Debug, Clone)]
pub struct General {
    pub gaps_in: i32,
    pub gaps_out: i32,
    pub border_size: i32,
    pub layout: LayoutKind,
    pub focus_follows_mouse: bool,
    pub col_active: [f32; 4],
    pub col_inactive: [f32; 4],
}

impl Default for General {
    fn default() -> Self {
        Self {
            gaps_in: 5,
            gaps_out: 10,
            border_size: 2,
            layout: LayoutKind::Dwindle,
            focus_follows_mouse: true,
            // eclipse amber on near-black
            col_active: [0.91, 0.64, 0.24, 1.0],
            col_inactive: [0.09, 0.09, 0.09, 1.0],
        }
    }
}

/// `clipboard { ... }` (COMP-06 §4).
#[derive(Debug, Clone, Default)]
pub struct Clipboard {
    /// Process names allowed to bind `zwlr_data_control_manager_v1`. Empty
    /// (the default) denies everyone — data control reads every selection.
    pub data_control_allow: Vec<String>,
}

/// One `output "<pattern>" { .. }` block (COMP-13 §4). Config wins over the
/// persisted layout.
#[derive(Debug, Clone, Default)]
pub struct OutputRule {
    /// Matched against the connector name and the output identity, `*` globbing.
    pub pattern: String,
    /// `WIDTHxHEIGHT` or `WIDTHxHEIGHT@REFRESH`, refresh in Hz or mHz.
    pub mode: Option<(i32, i32, i32)>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub transform: Option<String>,
    pub enabled: Option<bool>,
    pub vrr: Option<bool>,
}

/// `render { ... }` (COMP-02 §2).
#[derive(Debug, Clone)]
pub struct Render {
    /// Allow the DRM backend to hand buffers straight to KMS planes.
    /// Composition is still forced for any frame containing a surface
    /// flagged sensitive (COMP-02 §7).
    pub direct_scanout: bool,
}

impl Default for Render {
    fn default() -> Self {
        Self { direct_scanout: true }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub render: Render,
    pub clipboard: Clipboard,
    pub binds: Vec<Bind>,
    /// Per-workspace layout overrides, indexed 1..=10.
    pub workspace_layout: [Option<LayoutKind>; 10],
    /// `output` blocks in file order; the last match wins.
    pub outputs: Vec<OutputRule>,
    /// Files this config was built from, for the hot-reload stub.
    pub sources: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            render: Render::default(),
            clipboard: Clipboard::default(),
            binds: default_binds(),
            workspace_layout: Default::default(),
            outputs: Vec::new(),
            sources: Vec::new(),
        }
    }
}

fn m(logo: bool, shift: bool, ctrl: bool, alt: bool) -> Mods {
    Mods {
        logo,
        shift,
        ctrl,
        alt,
    }
}

/// Hyprland-ish defaults. `Super+Escape` is deliberately left unbound: it is
/// reserved for the trusted-UI override chord (COMP-04 §6).
pub fn default_binds() -> Vec<Bind> {
    let sup = m(true, false, false, false);
    let sup_shift = m(true, true, false, false);
    let mut b = vec![
        Bind {
            mods: sup,
            key: Keysym::Return,
            action: Action::Spawn("kitty".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::q,
            action: Action::Close,
        },
        Bind {
            mods: sup,
            key: Keysym::v,
            action: Action::ToggleFloating,
        },
        Bind {
            mods: sup,
            key: Keysym::e,
            action: Action::ToggleLayout,
        },
        Bind {
            mods: sup,
            key: Keysym::h,
            action: Action::Focus(Direction::Left),
        },
        Bind {
            mods: sup,
            key: Keysym::j,
            action: Action::Focus(Direction::Down),
        },
        Bind {
            mods: sup,
            key: Keysym::k,
            action: Action::Focus(Direction::Up),
        },
        Bind {
            mods: sup,
            key: Keysym::l,
            action: Action::Focus(Direction::Right),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::H,
            action: Action::Move(Direction::Left),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::J,
            action: Action::Move(Direction::Down),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::K,
            action: Action::Move(Direction::Up),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::L,
            action: Action::Move(Direction::Right),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Q,
            action: Action::Quit,
        },
    ];
    const DIGITS: [Keysym; 10] = [
        Keysym::_1,
        Keysym::_2,
        Keysym::_3,
        Keysym::_4,
        Keysym::_5,
        Keysym::_6,
        Keysym::_7,
        Keysym::_8,
        Keysym::_9,
        Keysym::_0,
    ];
    // Shifted digits on a US layout; both forms are accepted so the bind fires
    // whether or not the client layout shifts the symbol.
    const SHIFTED: [Keysym; 10] = [
        Keysym::exclam,
        Keysym::at,
        Keysym::numbersign,
        Keysym::dollar,
        Keysym::percent,
        Keysym::asciicircum,
        Keysym::ampersand,
        Keysym::asterisk,
        Keysym::parenleft,
        Keysym::parenright,
    ];
    for (i, key) in DIGITS.iter().enumerate() {
        b.push(Bind {
            mods: sup,
            key: *key,
            action: Action::SwitchWorkspace(i + 1),
        });
        b.push(Bind {
            mods: sup_shift,
            key: *key,
            action: Action::MoveToWorkspace(i + 1),
        });
        b.push(Bind {
            mods: sup_shift,
            key: SHIFTED[i],
            action: Action::MoveToWorkspace(i + 1),
        });
    }
    b
}

impl Config {
    /// Load from `explicit` if given, else from the search path. Errors are
    /// logged; the returned config is always usable.
    pub fn load(explicit: Option<&Path>) -> Self {
        let files: Vec<PathBuf> = match explicit {
            Some(p) => vec![p.to_path_buf()],
            None => search_path(),
        };
        let mut cfg = Config::default();
        let mut binds_from_file = Vec::new();
        let mut any = false;
        for f in &files {
            let text = match std::fs::read_to_string(f) {
                Ok(t) => t,
                Err(e) => {
                    if explicit.is_some() {
                        tracing::error!(path = %f.display(), %e, "config unreadable, using defaults");
                    }
                    continue;
                }
            };
            let doc = match text.parse::<KdlDocument>() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(path = %f.display(), error = %e, "config parse failed, file ignored");
                    continue;
                }
            };
            any = true;
            cfg.sources.push(f.clone());
            cfg.apply(&doc, &mut binds_from_file);
        }
        if !binds_from_file.is_empty() {
            cfg.binds = binds_from_file;
        }
        if !any {
            tracing::info!("no config found, using built-in defaults");
        } else {
            tracing::info!(sources = ?cfg.sources, binds = cfg.binds.len(), "config loaded");
        }
        cfg
    }

    /// Re-read the same sources. Hot-reload (inotify on the search path) is a
    /// TODO for M6; this is the reload entry point it will call.
    #[allow(dead_code)]
    pub fn reload(&self) -> Self {
        Self::load(self.sources.first().map(|p| p.as_path()))
    }

    fn apply(&mut self, doc: &KdlDocument, binds: &mut Vec<Bind>) {
        for node in doc.nodes() {
            match node.name().value() {
                "general" => self.apply_general(node),
                "bind" => match parse_bind(node) {
                    Ok(b) => binds.push(b),
                    Err(e) => tracing::warn!(error = %e, "ignoring bind"),
                },
                "workspace" => self.apply_workspace(node),
                "render" => self.apply_render(node),
                "clipboard" => self.apply_clipboard(node),
                "output" => self.apply_output(node),
                // Blocks specified but not implemented in M2.
                "decoration" | "animations" | "input" | "windowrule" => {}
                other => tracing::warn!(node = other, "unknown config node, ignored"),
            }
        }
    }

    fn apply_general(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "gaps-in" => set_i32(&mut self.general.gaps_in, n),
                "gaps-out" => set_i32(&mut self.general.gaps_out, n),
                "border-size" => set_i32(&mut self.general.border_size, n),
                "focus-follows-mouse" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.focus_follows_mouse = b;
                    }
                }
                "layout" => match arg(n).and_then(KdlValue::as_string) {
                    Some("dwindle") => self.general.layout = LayoutKind::Dwindle,
                    Some("master") => self.general.layout = LayoutKind::Master,
                    other => tracing::warn!(?other, "unknown layout, keeping default"),
                },
                "col-active-border" | "col-inactive-border" => {
                    match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                        Some(c) if name.starts_with("col-active") => self.general.col_active = c,
                        Some(c) => self.general.col_inactive = c,
                        None => tracing::warn!(node = name, "bad color, keeping default"),
                    }
                }
                other => tracing::warn!(node = other, "unknown general key, ignored"),
            }
        }
    }

    fn apply_render(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "direct-scanout" => {
                    self.render.direct_scanout = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                other => tracing::warn!(node = other, "unknown render node, ignored"),
            }
        }
    }

    fn apply_clipboard(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "data-control-allow" => {
                    self.clipboard.data_control_allow = args(n)
                        .into_iter()
                        .filter_map(|v| v.as_string().map(str::to_string))
                        .collect();
                }
                other => tracing::warn!(node = other, "unknown clipboard node, ignored"),
            }
        }
    }

    fn apply_workspace(&mut self, node: &KdlNode) {
        let Some(idx) = arg(node).and_then(KdlValue::as_integer) else {
            tracing::warn!("workspace node needs a number argument");
            return;
        };
        if !(1..=10).contains(&idx) {
            tracing::warn!(idx, "workspace out of range 1..=10, ignored");
            return;
        }
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() == "layout" {
                let slot = &mut self.workspace_layout[idx as usize - 1];
                match arg(n).and_then(KdlValue::as_string) {
                    Some("dwindle") => *slot = Some(LayoutKind::Dwindle),
                    Some("master") => *slot = Some(LayoutKind::Master),
                    other => tracing::warn!(?other, "unknown workspace layout"),
                }
            }
        }
    }

    fn apply_output(&mut self, node: &KdlNode) {
        let Some(pattern) = arg(node).and_then(KdlValue::as_string).map(str::to_owned) else {
            tracing::warn!("output node needs a name pattern argument");
            return;
        };
        let mut rule = OutputRule {
            pattern,
            ..Default::default()
        };
        let Some(children) = node.children() else {
            self.outputs.push(rule);
            return;
        };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "mode" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(crate::outputs::persist::parse_mode)
                {
                    Some(m) => rule.mode = Some(m),
                    None => tracing::warn!("bad output mode, expected \"1920x1080@60\""),
                },
                "position" => {
                    let a = n.entries();
                    match (
                        a.first().and_then(|e| e.value().as_integer()),
                        a.get(1).and_then(|e| e.value().as_integer()),
                    ) {
                        (Some(x), Some(y)) => rule.position = Some((x as i32, y as i32)),
                        _ => tracing::warn!("output position takes two integers"),
                    }
                }
                "scale" => match arg(n).and_then(as_f64) {
                    Some(s) if s > 0.0 => rule.scale = Some(s),
                    _ => tracing::warn!("output scale must be a positive number"),
                },
                "transform" => match arg(n).and_then(KdlValue::as_string) {
                    Some(t) if crate::outputs::parse_transform(t).is_some() => {
                        rule.transform = Some(t.to_owned())
                    }
                    other => tracing::warn!(?other, "unknown output transform"),
                },
                "enabled" | "disabled" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(v) => rule.enabled = Some(v == (name == "enabled")),
                    None => rule.enabled = Some(name == "enabled"),
                },
                "vrr" | "adaptive-sync" => rule.vrr = arg(n).and_then(KdlValue::as_bool).or(Some(true)),
                other => tracing::warn!(node = other, "unknown output key, ignored"),
            }
        }
        self.outputs.push(rule);
    }

    /// The effective rule for an output, later blocks overriding earlier ones.
    pub fn output_rule(&self, connector: &str, identity: &str) -> OutputRule {
        let mut out = OutputRule::default();
        for r in &self.outputs {
            if !(glob_match(&r.pattern, connector) || glob_match(&r.pattern, identity)) {
                continue;
            }
            out.pattern = r.pattern.clone();
            out.mode = r.mode.or(out.mode);
            out.position = r.position.or(out.position);
            out.scale = r.scale.or(out.scale);
            out.transform = r.transform.clone().or(out.transform);
            out.enabled = r.enabled.or(out.enabled);
            out.vrr = r.vrr.or(out.vrr);
        }
        out
    }

    pub fn layout_for(&self, workspace: usize) -> LayoutKind {
        self.workspace_layout
            .get(workspace.saturating_sub(1))
            .copied()
            .flatten()
            .unwrap_or(self.general.layout)
    }

    pub fn action_for(&self, mods: &ModifiersState, key: Keysym) -> Option<&Action> {
        self.binds
            .iter()
            .find(|b| b.key == key && b.mods.matches(mods))
            .map(|b| &b.action)
    }
}

fn search_path() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/etc/eclipse/helios.kdl")];
    let cfg_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    let Some(base) = cfg_home else { return out };
    out.push(base.join("eclipse/helios.kdl"));
    if let Ok(dir) = std::fs::read_dir(base.join("eclipse/helios.d")) {
        let mut drop_ins: Vec<PathBuf> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
            .collect();
        drop_ins.sort();
        out.extend(drop_ins);
    }
    // Compatibility with the path used by the M2 task brief.
    out.push(base.join("helios/config.kdl"));
    out
}

fn arg(node: &KdlNode) -> Option<&KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .map(|e| e.value())
}

fn args(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

fn set_i32(slot: &mut i32, node: &KdlNode) {
    match arg(node).and_then(KdlValue::as_integer) {
        Some(v) => *slot = v.clamp(0, 512) as i32,
        None => tracing::warn!(node = node.name().value(), "expected an integer"),
    }
}

/// `#rrggbb`, `#rrggbbaa`, `0xaarrggbb` (Hyprland's form) or `rrggbb`.
fn parse_color(s: &str) -> Option<[f32; 4]> {
    let t = s.trim();
    let (hex, argb) = if let Some(h) = t.strip_prefix('#') {
        (h, false)
    } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (h, h.len() == 8)
    } else {
        (t, false)
    };
    let v = u32::from_str_radix(hex, 16).ok()?;
    let f = |b: u32| (b & 0xff) as f32 / 255.0;
    Some(match (hex.len(), argb) {
        (6, _) => [f(v >> 16), f(v >> 8), f(v), 1.0],
        (8, true) => [f(v >> 16), f(v >> 8), f(v), f(v >> 24)],
        (8, false) => [f(v >> 24), f(v >> 16), f(v >> 8), f(v)],
        _ => return None,
    })
}

fn parse_mods(s: &str) -> Result<Mods, String> {
    let mut mods = Mods::default();
    for part in s.split(['+', ' ', ',']).filter(|p| !p.is_empty()) {
        match part.to_ascii_uppercase().as_str() {
            "SUPER" | "MOD" | "LOGO" | "MOD4" => mods.logo = true,
            "SHIFT" => mods.shift = true,
            "CTRL" | "CONTROL" => mods.ctrl = true,
            "ALT" | "MOD1" => mods.alt = true,
            "NONE" | "" => {}
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(mods)
}

fn parse_keysym(s: &str) -> Result<Keysym, String> {
    let k = xkb::keysym_from_name(s, xkb::KEYSYM_NO_FLAGS);
    let k = if k == Keysym::NoSymbol {
        xkb::keysym_from_name(s, xkb::KEYSYM_CASE_INSENSITIVE)
    } else {
        k
    };
    if k == Keysym::NoSymbol {
        Err(format!("unknown key '{s}'"))
    } else {
        Ok(k)
    }
}

/// `bind "SUPER" "Return" { spawn "kitty"; }`
fn parse_bind(node: &KdlNode) -> Result<Bind, String> {
    let a = args(node);
    let (mods, key) = match a.len() {
        1 => (Mods::default(), a[0].as_string().ok_or("key must be a string")?),
        2 => (
            parse_mods(a[0].as_string().ok_or("modifiers must be a string")?)?,
            a[1].as_string().ok_or("key must be a string")?,
        ),
        _ => return Err("bind takes [modifiers] key { action }".into()),
    };
    let key = parse_keysym(key)?;
    let children = node.children().ok_or("bind needs an action block")?;
    let action_node = children.nodes().first().ok_or("bind action block is empty")?;
    let action = parse_action(action_node)?;
    if mods == m(true, false, false, false) && key == Keysym::Escape {
        return Err("Super+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    Ok(Bind { mods, key, action })
}

fn parse_action(node: &KdlNode) -> Result<Action, String> {
    let a = args(node);
    let text = || a.first().and_then(|v| v.as_string()).map(str::to_owned);
    let num = || a.first().and_then(|v| v.as_integer());
    Ok(match node.name().value() {
        "spawn" | "exec" => Action::Spawn(text().ok_or("spawn needs a command string")?),
        "close-window" | "killactive" => Action::Close,
        "toggle-floating" => Action::ToggleFloating,
        "toggle-layout" => Action::ToggleLayout,
        "focus-left" => Action::Focus(Direction::Left),
        "focus-right" => Action::Focus(Direction::Right),
        "focus-up" => Action::Focus(Direction::Up),
        "focus-down" => Action::Focus(Direction::Down),
        "move-left" => Action::Move(Direction::Left),
        "move-right" => Action::Move(Direction::Right),
        "move-up" => Action::Move(Direction::Up),
        "move-down" => Action::Move(Direction::Down),
        "workspace" => Action::SwitchWorkspace(workspace_arg(num())?),
        "move-to-workspace" => Action::MoveToWorkspace(workspace_arg(num())?),
        "quit" | "exit" => Action::Quit,
        other => return Err(format!("unknown action '{other}'")),
    })
}

fn workspace_arg(n: Option<i128>) -> Result<usize, String> {
    match n {
        Some(v) if (1..=10).contains(&v) => Ok(v as usize),
        _ => Err("workspace number must be 1..=10".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("0x80ff0000"), Some([1.0, 0.0, 0.0, 0.5019608]));
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn parses_general_and_binds() {
        let doc: KdlDocument = r#"
            general { gaps-in 3; layout "master"; border-size 4 }
            bind "SUPER SHIFT" "Return" { spawn "foot"; }
            workspace 2 { layout "dwindle" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.general.gaps_in, 3);
        assert_eq!(cfg.general.border_size, 4);
        assert_eq!(cfg.general.layout, LayoutKind::Master);
        assert_eq!(cfg.layout_for(2), LayoutKind::Dwindle);
        assert_eq!(binds.len(), 1);
        assert!(matches!(binds[0].action, Action::Spawn(ref c) if c == "foot"));
        assert!(binds[0].mods.shift && binds[0].mods.logo);
    }

    #[test]
    fn bad_nodes_are_skipped_not_fatal() {
        let doc: KdlDocument = "general { layout \"bogus\" }\nbind \"SUPER\" \"Escape\" { quit; }\n"
            .parse()
            .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.general.layout, LayoutKind::Dwindle);
        assert!(binds.is_empty(), "Super+Escape must stay reserved");
    }
}

fn as_f64(v: &KdlValue) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// `*` matches any run of characters; everything else is literal. Anchored.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return pattern == text;
    };
    if !text.starts_with(first) {
        return false;
    }
    if !pattern.contains('*') {
        return text.len() == first.len();
    }
    let mut rest = &text[first.len()..];
    let parts: Vec<&str> = parts.collect();
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        if i + 1 == parts.len() && !pattern.ends_with('*') {
            return rest.ends_with(p) && rest.len() >= p.len();
        }
        match rest.find(p) {
            Some(at) => rest = &rest[at + p.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("DP-1", "DP-1"));
        assert!(!glob_match("DP-1", "DP-11"));
        assert!(glob_match("eDP-*", "eDP-1"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*Dell*", "Dell U2720Q ABC"));
        assert!(!glob_match("*Dell*", "LG U2720Q"));
    }

    #[test]
    fn later_blocks_win() {
        let mut c = Config::default();
        let doc: KdlDocument = "output \"*\" { scale 1.0 }\noutput \"DP-*\" { scale 2.0; position 100 0 }"
            .parse()
            .unwrap();
        let mut binds = Vec::new();
        c.apply(&doc, &mut binds);
        let r = c.output_rule("DP-1", "Dell X Y");
        assert_eq!(r.scale, Some(2.0));
        assert_eq!(r.position, Some((100, 0)));
        assert_eq!(c.output_rule("HDMI-A-1", "x").scale, Some(1.0));
    }
}
