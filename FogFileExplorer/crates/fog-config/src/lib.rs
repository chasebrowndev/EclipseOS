// SPDX-License-Identifier: AGPL-3.0-only

//! Fog's configuration (FOG §Configuration): one KDL file,
//! `$XDG_CONFIG_HOME/eclipse/fog.kdl`, parsed into typed structs and laid
//! over the shipped `fog.default.kdl`, which is embedded here so the defaults
//! have one source of truth.
//!
//! Unknown nodes, keys and values are errors carrying a 1-based line and
//! column. [`Live`] keeps the last valid config when a reload fails.

mod keys;
pub mod theme;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

pub use keys::{Action, Chord, Key, Mods, Target};

/// The shipped defaults, every option listed.
pub const DEFAULT_KDL: &str = include_str!("../../../config/fog.default.kdl");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub view: View,
    pub appearance: Appearance,
    pub performance: Performance,
    /// Chord to what it runs; a user `bind` replaces the default per chord.
    pub keys: BTreeMap<Chord, Target>,
    pub agents: Agents,
    pub actions: Vec<CustomAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub default: ViewMode,
    pub show_hidden: bool,
    pub sort: SortKey,
    pub natural: bool,
    pub dirs_first: bool,
}

/// `appearance { … }` (FOG §Visual design, accessibility).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Appearance {
    /// Springs snap straight to their target.
    pub reduce_motion: bool,
    /// No blur; every glass tint is opaque.
    pub reduce_transparency: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
    Columns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Performance {
    pub cache_dirs: usize,
    pub cache_mib: usize,
    pub prefetch_hover_ms: u64,
    pub thumbnail_workers: Workers,
}

impl Performance {
    pub fn cache_bytes(&self) -> usize {
        self.cache_mib << 20
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workers {
    /// CPU cores minus one.
    Auto,
    Count(usize),
}

/// Agent operations that need a human confirmation click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentOp {
    Replace,
    Trash,
    Move,
    Copy,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agents {
    /// Always contains [`AgentOp::Replace`]: config only tightens the floor.
    pub confirm: BTreeSet<AgentOp>,
}

/// `action "<name>" key="<chord>" { run "<cmd>" "<arg>"… }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAction {
    pub name: String,
    pub key: Option<Chord>,
    pub run: Vec<String>,
}

/// A rejected config. `line` and `col` are 1-based; both are 0 when the
/// file could not be read at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("fog.kdl:{line}:{col}: {msg}")]
pub struct Error {
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

/// The shipped defaults.
pub fn defaults() -> Config {
    Config::blank()
        .overlay(DEFAULT_KDL)
        .expect("fog.default.kdl parses (tested)")
}

/// `text` laid over the defaults.
pub fn parse(text: &str) -> Result<Config, Error> {
    defaults().overlay(text)
}

/// Load `path` over the defaults. A missing file is the defaults.
pub fn load(path: &Path) -> Result<Config, Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(defaults()),
        Err(e) => Err(Error {
            line: 0,
            col: 0,
            msg: format!("cannot read {}: {e}", path.display()),
        }),
    }
}

/// `$XDG_CONFIG_HOME/eclipse/fog.kdl`, falling back to `~/.config`.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })?;
    Some(base.join("eclipse").join("fog.kdl"))
}

/// The active config. A failed reload leaves it untouched.
#[derive(Debug, Clone)]
pub struct Live {
    current: Config,
}

impl Live {
    pub fn new(current: Config) -> Live {
        Live { current }
    }

    pub fn current(&self) -> &Config {
        &self.current
    }

    /// Adopt `next` if it parsed. `Ok(true)` when the config changed.
    pub fn apply(&mut self, next: Result<Config, Error>) -> Result<bool, Error> {
        let next = next?;
        let changed = next != self.current;
        self.current = next;
        Ok(changed)
    }

    /// Re-read `path`; see [`Live::apply`].
    pub fn reload(&mut self, path: &Path) -> Result<bool, Error> {
        self.apply(load(path))
    }
}

impl Config {
    /// The base the default file is laid over; never exposed.
    fn blank() -> Config {
        Config {
            view: View {
                default: ViewMode::List,
                show_hidden: false,
                sort: SortKey::Name,
                natural: false,
                dirs_first: false,
            },
            appearance: Appearance {
                reduce_motion: false,
                reduce_transparency: false,
            },
            performance: Performance {
                cache_dirs: 1,
                cache_mib: 1,
                prefetch_hover_ms: 0,
                thumbnail_workers: Workers::Auto,
            },
            keys: BTreeMap::new(),
            agents: Agents {
                confirm: BTreeSet::from([AgentOp::Replace]),
            },
            actions: Vec::new(),
        }
    }

    /// A copy of `self` with `text` applied on top.
    pub fn overlay(&self, text: &str) -> Result<Config, Error> {
        let cx = Cx { text };
        let doc = text.parse::<KdlDocument>().map_err(|e| {
            let (off, msg) = match e.diagnostics.first() {
                Some(d) => {
                    let mut m = d
                        .message
                        .clone()
                        .unwrap_or_else(|| "invalid syntax".to_string());
                    if let Some(help) = &d.help {
                        m.push_str(&format!(" ({help})"));
                    }
                    (d.span.offset(), m)
                }
                None => (0, e.to_string()),
            };
            cx.at(off, msg)
        })?;
        let mut cfg = self.clone();
        cfg.apply(&doc, &cx)?;
        Ok(cfg)
    }

    fn apply(&mut self, doc: &KdlDocument, cx: &Cx) -> Result<(), Error> {
        // Custom actions first, so `bind` can name one defined further down.
        for node in doc.nodes() {
            match node.name().value() {
                "view" => self.view(node, cx)?,
                "appearance" => self.appearance(node, cx)?,
                "performance" => self.performance(node, cx)?,
                "agents" => self.agents(node, cx)?,
                "activity" => activity(node, cx)?,
                "action" => self.action(node, cx)?,
                "keys" => {}
                other => return Err(cx.node(node, format!("unknown node `{other}`"))),
            }
        }
        for node in doc.nodes().iter().filter(|n| n.name().value() == "keys") {
            self.keys(node, cx)?;
        }
        Ok(())
    }

    fn view(&mut self, node: &KdlNode, cx: &Cx) -> Result<(), Error> {
        for n in section(node, cx)? {
            match n.name().value() {
                "default" => {
                    let e = single(n, cx)?;
                    self.view.default = match string(e, cx)? {
                        "list" => ViewMode::List,
                        "grid" => ViewMode::Grid,
                        "columns" => ViewMode::Columns,
                        v => return Err(cx.entry(e, format!("unknown view `{v}`"))),
                    };
                }
                "show-hidden" => self.view.show_hidden = boolean(single(n, cx)?, cx)?,
                "sort" => {
                    let (pos, props) = split(n);
                    let [e] = pos[..] else {
                        return Err(cx.node(n, "`sort` needs one sort key"));
                    };
                    self.view.sort = match string(e, cx)? {
                        "name" => SortKey::Name,
                        "size" => SortKey::Size,
                        "modified" => SortKey::Modified,
                        "type" => SortKey::Type,
                        v => return Err(cx.entry(e, format!("unknown sort key `{v}`"))),
                    };
                    for (k, e) in props {
                        match k {
                            "natural" => self.view.natural = boolean(e, cx)?,
                            "dirs-first" => self.view.dirs_first = boolean(e, cx)?,
                            _ => return Err(cx.entry(e, format!("unknown sort option `{k}`"))),
                        }
                    }
                }
                k => return Err(cx.node(n, format!("unknown key `{k}` in `view`"))),
            }
        }
        Ok(())
    }

    fn appearance(&mut self, node: &KdlNode, cx: &Cx) -> Result<(), Error> {
        let a = &mut self.appearance;
        for n in section(node, cx)? {
            match n.name().value() {
                "reduce-motion" => a.reduce_motion = boolean(single(n, cx)?, cx)?,
                "reduce-transparency" => a.reduce_transparency = boolean(single(n, cx)?, cx)?,
                k => return Err(cx.node(n, format!("unknown key `{k}` in `appearance`"))),
            }
        }
        Ok(())
    }

    fn performance(&mut self, node: &KdlNode, cx: &Cx) -> Result<(), Error> {
        let p = &mut self.performance;
        for n in section(node, cx)? {
            match n.name().value() {
                "cache-dirs" => p.cache_dirs = int(single(n, cx)?, 1, 1 << 20, cx)? as usize,
                "cache-mib" => p.cache_mib = int(single(n, cx)?, 1, 1 << 16, cx)? as usize,
                "prefetch-hover-ms" => p.prefetch_hover_ms = int(single(n, cx)?, 0, 10_000, cx)?,
                "thumbnail-workers" => {
                    let e = single(n, cx)?;
                    p.thumbnail_workers = match e.value().as_string() {
                        Some("auto") => Workers::Auto,
                        Some(_) => return Err(cx.entry(e, "expected \"auto\" or a count")),
                        None => Workers::Count(int(e, 1, 256, cx)? as usize),
                    };
                }
                k => return Err(cx.node(n, format!("unknown key `{k}` in `performance`"))),
            }
        }
        Ok(())
    }

    fn agents(&mut self, node: &KdlNode, cx: &Cx) -> Result<(), Error> {
        for n in section(node, cx)? {
            match n.name().value() {
                "confirm" => {
                    let (pos, props) = split(n);
                    if let Some((_, e)) = props.first() {
                        return Err(cx.entry(e, "unexpected property"));
                    }
                    if pos.is_empty() {
                        return Err(cx.node(n, "`confirm` needs at least one operation"));
                    }
                    // The floor stays: config can only tighten it.
                    let mut set = BTreeSet::from([AgentOp::Replace]);
                    for e in pos {
                        set.insert(match string(e, cx)? {
                            "replace" => AgentOp::Replace,
                            "trash" => AgentOp::Trash,
                            "move" => AgentOp::Move,
                            "copy" => AgentOp::Copy,
                            "rename" => AgentOp::Rename,
                            v => return Err(cx.entry(e, format!("unknown operation `{v}`"))),
                        });
                    }
                    self.agents.confirm = set;
                }
                k => return Err(cx.node(n, format!("unknown key `{k}` in `agents`"))),
            }
        }
        Ok(())
    }

    fn action(&mut self, node: &KdlNode, cx: &Cx) -> Result<(), Error> {
        let (pos, props) = split(node);
        let [e] = pos[..] else {
            return Err(cx.node(node, "`action` needs one name"));
        };
        let name = string(e, cx)?;
        if Action::from_name(name).is_some() {
            return Err(cx.entry(e, format!("`{name}` is a built-in action")));
        }
        let mut key = None;
        for (k, e) in props {
            match k {
                "key" => key = Some(chord(e, cx)?),
                _ => return Err(cx.entry(e, format!("unknown action option `{k}`"))),
            }
        }
        let mut run = None;
        for n in node.children().map(|d| d.nodes()).unwrap_or_default() {
            match n.name().value() {
                "run" if run.is_none() => {
                    let (pos, props) = split(n);
                    if let Some((_, e)) = props.first() {
                        return Err(cx.entry(e, "unexpected property"));
                    }
                    if pos.is_empty() {
                        return Err(cx.node(n, "`run` needs a command"));
                    }
                    let argv = pos
                        .into_iter()
                        .map(|e| string(e, cx).map(str::to_string))
                        .collect::<Result<Vec<_>, _>>()?;
                    run = Some(argv);
                }
                "run" => return Err(cx.node(n, "`run` given twice")),
                k => return Err(cx.node(n, format!("unknown key `{k}` in `action`"))),
            }
        }
        let Some(run) = run else {
            return Err(cx.node(node, "`action` needs a `run` command"));
        };
        self.actions.retain(|a| a.name != name);
        if let Some(k) = key {
            self.keys.insert(k, Target::Custom(name.to_string()));
        }
        self.actions.push(CustomAction {
            name: name.to_string(),
            key,
            run,
        });
        Ok(())
    }

    fn keys(&mut self, node: &KdlNode, cx: &Cx) -> Result<(), Error> {
        for n in section(node, cx)? {
            if n.name().value() != "bind" {
                return Err(cx.node(n, format!("unknown key `{}` in `keys`", n.name().value())));
            }
            let (pos, props) = split(n);
            if let Some((_, e)) = props.first() {
                return Err(cx.entry(e, "unexpected property"));
            }
            let [c, a] = pos[..] else {
                return Err(cx.node(n, "`bind` needs a chord and an action"));
            };
            let chord = chord(c, cx)?;
            let name = string(a, cx)?;
            let target = match Action::from_name(name) {
                Some(act) => Target::Action(act),
                None if self.actions.iter().any(|x| x.name == name) => {
                    Target::Custom(name.to_string())
                }
                None => return Err(cx.entry(a, format!("unknown action `{name}`"))),
            };
            self.keys.insert(chord, target);
        }
        Ok(())
    }
}

/// `activity { … }` is used only by the activity-lens hook: shape-checked,
/// then ignored.
fn activity(node: &KdlNode, cx: &Cx) -> Result<(), Error> {
    for n in section(node, cx)? {
        let e = single(n, cx)?;
        match n.name().value() {
            "user-lens-marks" | "notify-task-end" => {
                boolean(e, cx)?;
            }
            "default-view" => {
                string(e, cx)?;
            }
            "heat-decay-s" | "retention-days" => {
                int(e, 0, u32::MAX as u64, cx)?;
            }
            k => return Err(cx.node(n, format!("unknown key `{k}` in `activity`"))),
        }
    }
    Ok(())
}

/// Source text, for turning spans into line and column.
struct Cx<'a> {
    text: &'a str,
}

impl Cx<'_> {
    fn at(&self, offset: usize, msg: impl Into<String>) -> Error {
        let off = offset.min(self.text.len());
        let head = &self.text[..off];
        let start = head.rfind('\n').map_or(0, |i| i + 1);
        Error {
            line: head.matches('\n').count() as u32 + 1,
            col: self.text[start..off].chars().count() as u32 + 1,
            msg: msg.into(),
        }
    }

    fn node(&self, n: &KdlNode, msg: impl Into<String>) -> Error {
        self.at(n.name().span().offset(), msg)
    }

    /// An entry's span may start at the whitespace before it.
    fn entry(&self, e: &KdlEntry, msg: impl Into<String>) -> Error {
        let s = e.span();
        let raw = self
            .text
            .get(s.offset()..s.offset() + s.len())
            .unwrap_or("");
        self.at(s.offset() + raw.len() - raw.trim_start().len(), msg)
    }
}

/// A section node: no arguments, returns its children.
fn section<'a>(node: &'a KdlNode, cx: &Cx) -> Result<&'a [KdlNode], Error> {
    if let Some(e) = node.entries().first() {
        let what = node.name().value();
        return Err(cx.entry(e, format!("`{what}` takes no arguments")));
    }
    Ok(node.children().map(|d| d.nodes()).unwrap_or_default())
}

type Props<'a> = Vec<(&'a str, &'a KdlEntry)>;

fn split(node: &KdlNode) -> (Vec<&KdlEntry>, Props<'_>) {
    let mut pos = Vec::new();
    let mut props = Vec::new();
    for e in node.entries() {
        match e.name() {
            Some(k) => props.push((k.value(), e)),
            None => pos.push(e),
        }
    }
    (pos, props)
}

/// Exactly one argument and no properties or children.
fn single<'a>(n: &'a KdlNode, cx: &Cx) -> Result<&'a KdlEntry, Error> {
    let what = n.name().value();
    if n.children().is_some() {
        return Err(cx.node(n, format!("`{what}` takes no block")));
    }
    match n.entries() {
        [e] if e.name().is_none() => Ok(e),
        [] => Err(cx.node(n, format!("`{what}` needs a value"))),
        [e] => Err(cx.entry(e, "unexpected property")),
        [_, e, ..] => Err(cx.entry(e, format!("`{what}` takes one value"))),
    }
}

fn string<'a>(e: &'a KdlEntry, cx: &Cx) -> Result<&'a str, Error> {
    e.value()
        .as_string()
        .ok_or_else(|| cx.entry(e, "expected a string"))
}

fn boolean(e: &KdlEntry, cx: &Cx) -> Result<bool, Error> {
    e.value()
        .as_bool()
        .ok_or_else(|| cx.entry(e, "expected #true or #false"))
}

fn int(e: &KdlEntry, min: u64, max: u64, cx: &Cx) -> Result<u64, Error> {
    match e.value() {
        KdlValue::Integer(i) if (min as i128..=max as i128).contains(i) => Ok(*i as u64),
        KdlValue::Integer(_) => Err(cx.entry(e, format!("expected {min} to {max}"))),
        _ => Err(cx.entry(e, "expected an integer")),
    }
}

fn chord(e: &KdlEntry, cx: &Cx) -> Result<Chord, Error> {
    Chord::parse(string(e, cx)?).map_err(|m| cx.entry(e, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(s: &str) -> Chord {
        Chord::parse(s).unwrap()
    }

    #[test]
    fn defaults_parse_and_bind_every_action() {
        let d = defaults();
        assert_eq!(d.view.default, ViewMode::List);
        assert!(!d.view.show_hidden);
        assert_eq!(d.view.sort, SortKey::Name);
        assert!(d.view.natural && d.view.dirs_first);
        assert_eq!(
            d.performance,
            Performance {
                cache_dirs: 256,
                cache_mib: 64,
                prefetch_hover_ms: 120,
                thumbnail_workers: Workers::Auto,
            }
        );
        assert_eq!(d.performance.cache_bytes(), 64 << 20);
        assert_eq!(d.agents.confirm, BTreeSet::from([AgentOp::Replace]));
        assert!(d.actions.is_empty());
        for a in Action::ALL {
            assert!(
                d.keys.values().any(|t| *t == Target::Action(a)),
                "{} has no default binding",
                a.name()
            );
        }
        for (c, a) in [
            ("ctrl+p", Action::Palette),
            (":", Action::Palette),
            ("space", Action::QuickLook),
            ("shift+delete", Action::Delete),
            ("delete", Action::Trash),
            ("ctrl+shift+n", Action::NewFolder),
            ("G", Action::Bottom),
            ("f2", Action::Rename),
        ] {
            assert_eq!(d.keys.get(&bind(c)), Some(&Target::Action(a)), "{c}");
        }
    }

    #[test]
    fn appearance_keys_parse_and_reject_strangers() {
        let d = defaults();
        assert!(!d.appearance.reduce_motion && !d.appearance.reduce_transparency);
        let c = parse("appearance {\n  reduce-motion #true\n}").unwrap();
        assert!(c.appearance.reduce_motion && !c.appearance.reduce_transparency);
        let c = parse("appearance { reduce-transparency #true }").unwrap();
        assert!(c.appearance.reduce_transparency);
        let e = parse("appearance {\n  reduce-blur #true\n}").unwrap_err();
        assert_eq!((e.line, e.col), (2, 3), "{e}");
        assert!(parse("appearance { reduce-motion \"yes\" }").is_err());
        assert!(parse("appearance #true").is_err());
    }

    #[test]
    fn unknown_key_reports_line_and_col() {
        let e = parse("view {\n    default \"grid\"\n    colour \"gold\"\n}\n").unwrap_err();
        assert_eq!((e.line, e.col), (3, 5), "{e}");
        assert!(e.msg.contains("colour"), "{e}");

        let e = parse("performance {\n  cache-dirs 12 extra=1\n}").unwrap_err();
        assert_eq!((e.line, e.col), (2, 17), "{e}");

        let e = parse("\nbogus 1").unwrap_err();
        assert_eq!((e.line, e.col), (2, 1), "{e}");

        let e = parse("view { sort \"name\" reverse=#true }").unwrap_err();
        assert_eq!((e.line, e.col), (1, 20), "{e}");

        // Syntax errors carry a position too.
        let e = parse("view {\n  default \"list\n").unwrap_err();
        assert!(e.line >= 2, "{e}");
    }

    #[test]
    fn bad_values_are_errors() {
        for bad in [
            "view { default \"tree\" }",
            "view { show-hidden \"yes\" }",
            "performance { cache-dirs 0 }",
            "performance { thumbnail-workers \"many\" }",
            "agents { confirm \"everything\" }",
            "keys { bind \"ctrl+p\" \"launch-missiles\" }",
            "keys { bind \"hyper+p\" \"palette\" }",
            "keys { bind \"esc\" \"palette\" }",
            "keys { unbind \"ctrl+p\" }",
            "action \"trash\" { run \"rm\" }",
            "action \"x\" key=\"ctrl+y\"",
            "activity { lens-colour \"red\" }",
            "view \"list\"",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn user_file_overrides_and_merges() {
        let c = parse(
            "view { show-hidden #true }\n\
             performance { cache-dirs 32; thumbnail-workers 3 }\n\
             keys {\n  bind \"ctrl+p\" \"undo\"\n  bind \"x\" \"term\"\n}\n\
             agents { confirm \"trash\" }\n\
             action \"term\" key=\"ctrl+alt+t\" { run \"cataclysm\" \"--cwd\" \"{cwd}\" }\n",
        )
        .unwrap();
        let d = defaults();
        // Only what the file names changes.
        assert!(c.view.show_hidden);
        assert_eq!(c.view.sort, d.view.sort);
        assert_eq!(c.performance.cache_dirs, 32);
        assert_eq!(c.performance.cache_mib, 64);
        assert_eq!(c.performance.thumbnail_workers, Workers::Count(3));
        // `bind` overrides per chord; the rest of the defaults stay.
        assert_eq!(
            c.keys.get(&bind("ctrl+p")),
            Some(&Target::Action(Action::Undo))
        );
        assert_eq!(
            c.keys.get(&bind(":")),
            Some(&Target::Action(Action::Palette))
        );
        assert_eq!(c.keys.len(), d.keys.len() + 2);
        // A binding may name a custom action defined later in the file.
        assert_eq!(c.keys.get(&bind("x")), Some(&Target::Custom("term".into())));
        assert_eq!(
            c.keys.get(&bind("ctrl+alt+t")),
            Some(&Target::Custom("term".into()))
        );
        assert_eq!(c.actions[0].run, ["cataclysm", "--cwd", "{cwd}"]);
        // Tightening keeps the floor.
        assert_eq!(
            c.agents.confirm,
            BTreeSet::from([AgentOp::Replace, AgentOp::Trash])
        );
        assert_eq!(parse("").unwrap(), d);
    }

    #[test]
    fn invalid_reload_keeps_old_config() {
        let dir = std::env::temp_dir().join(format!("fog-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fog.kdl");

        let mut live = Live::new(defaults());
        assert_eq!(
            live.reload(&path),
            Ok(false),
            "missing file is the defaults"
        );

        std::fs::write(&path, "view { show-hidden #true }").unwrap();
        assert_eq!(live.reload(&path), Ok(true));
        assert!(live.current().view.show_hidden);

        std::fs::write(&path, "view { show-hidden #true }\nnope 1\n").unwrap();
        let e = live.reload(&path).unwrap_err();
        assert_eq!((e.line, e.col), (2, 1));
        assert!(live.current().view.show_hidden, "old config stays active");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
