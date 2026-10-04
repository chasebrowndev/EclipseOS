// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-ctl config migrate` — split a legacy single `abyss.kdl` into the
//! two files COMP-13 §1.3 expects: `abyss.kdl` and `policy.kdl`. It also
//! moves the built-in applet ids out of `bar.tray` into `bar.widgets.order`
//! (ADR 0065; see [`migrate_widgets`]), and rewrites the legacy animation
//! forms as presets and per-event blocks (see [`migrate_animations`]).
//!
//! Three things make this local file surgery rather than an RPC:
//!
//! 1. It writes `policy.kdl`, and the control socket may never write that file
//!    — that refusal is the whole point of the split, and a migration verb
//!    that went through the socket would be the hole in it.
//! 2. It must work with the compositor stopped, which is when someone
//!    upgrading is most likely to run it.
//! 3. It is a one-shot rewrite of a whole document, so unlike the write path
//!    (ADR 0036) it is allowed to use the KDL *document* model. Nodes are
//!    moved between documents whole, which carries each node's own leading
//!    trivia — comments and blank lines stay attached to what they describe.
//!
//! Safety: both files are backed up to `.bak`, and the result is re-parsed
//! before anything is committed. If either side fails to parse, nothing is
//! written and the originals are untouched.

use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};

/// Dotted paths the schema marks `Owner::Policy`
/// (`ec_abyss_config::schema::TABLE`). Mirrored here so this CLI does not link
/// the compositor; `tests/schema_drift.rs` fails if the two ever diverge.
const POLICY_KEYS: &[&str] = &[
    "misc.scripted-input",
    "clipboard.data-control-allow",
    "capture.allow",
    "capture.redact-app-id",
    "capture.hide-layer",
];

/// `windowrule` actions the schema marks `Owner::Policy`
/// (`ec_abyss_config::schema::RULE_ACTIONS`). A rule carrying both kinds is
/// split in two, one node per file, keeping the same match criteria.
const POLICY_RULE_ACTIONS: &[&str] = &[
    "sensitivity",
    "app-trust",
    "seat-compat",
    "no-agent",
    "irreversible-capable",
];

/// Built-in applet ids `bar.tray.pinned`/`hidden` carried before they became
/// taskbar widgets (ADR 0065). Mirrors `ec_abyss_config::schema::LEGACY_TRAY_BUILTINS`;
/// `tests/schema_drift.rs` pins both copies.
const LEGACY_TRAY_BUILTINS: &[&str] = &["network", "bluetooth", "battery", "volume"];

/// `ec_abyss_config::schema::BAR_WIDGET_DEFAULT_ORDER`. The migrated order is
/// this list with the legacy ids rearranged by the old tray lists.
const BAR_WIDGET_DEFAULT_ORDER: &[&str] = &[
    "now-playing",
    "volume",
    "network",
    "bluetooth",
    "battery",
    "tray",
    "clock",
];

/// `ec_abyss_config::schema::LEGACY_ANIMATION_MAP`: legacy `animation` name
/// -> (event, style). Mirrored; pinned by `tests/schema_drift.rs`.
const LEGACY_ANIMATION_MAP: &[(&str, &str, &str)] = &[
    ("windows", "window-move", "glide"),
    ("workspaces", "workspace-switch", "slide"),
    ("fade", "window-open", "fade"),
    ("border", "focus", "crossfade"),
];

/// `schema::EASING_CURVES`: the only curves a legacy `animation` took.
const EASING_CURVES: &[&str] = &["linear", "ease-in", "ease-out", "ease-in-out"];

/// `schema::ANIMATION_CURVES`: what `bar.motion.curve` and an event's
/// `curve` take.
const ANIMATION_CURVES: &[&str] = &["linear", "ease-in", "ease-out", "ease-in-out", "spring", "bounce"];

/// `schema::{ANIMATION_DEFAULT_MS, ANIMATION_DEFAULT_CURVE, ANIMATION_MAX_MS}`.
const ANIMATION_DEFAULT_MS: u32 = 150;
const ANIMATION_DEFAULT_CURVE: &str = "ease-out";
const ANIMATION_MAX_MS: u32 = 10_000;

/// The top of `bar.motion.duration-ms` in the schema.
const BAR_MOTION_MAX_MS: i128 = 2000;

const POLICY_HEADER: &str = "\
// policy.kdl — the security surface (COMP-13 §1.3).
//
// Split out of abyss.kdl by `ec-ctl config migrate`. The control socket
// can read these settings but never writes them: changing one is a decision a
// human makes in this file, with a text editor.

";

fn config_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|b| b.join("eclipse"))
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())
}

/// Which file a top-level node's child belongs in.
fn child_is_policy(node: &str, child: &KdlNode) -> bool {
    let path = format!("{node}.{}", child.name().value());
    POLICY_KEYS.contains(&path.as_str())
}

fn rule_action_is_policy(child: &KdlNode) -> bool {
    POLICY_RULE_ACTIONS.contains(&child.name().value())
}

/// Split `node` into its abyss half and its policy half, by the predicate.
/// `None` on a side means that side has nothing and the node does not appear
/// in that file at all — a node with no children left behind would be a
/// silent semantic change (`capture {}` is not the same as no `capture`).
fn split_node(node: &KdlNode, is_policy: impl Fn(&KdlNode) -> bool) -> (Option<KdlNode>, Option<KdlNode>) {
    let Some(children) = node.children() else {
        return (Some(node.clone()), None);
    };
    let mut keep = node.clone();
    let mut moved = node.clone();
    let (mut n_keep, mut n_moved) = (0usize, 0usize);
    let keep_doc = keep.children_mut().as_mut().expect("children");
    keep_doc.nodes_mut().retain(|c| {
        let policy = is_policy(c);
        n_keep += usize::from(!policy);
        !policy
    });
    let moved_doc = moved.children_mut().as_mut().expect("children");
    moved_doc.nodes_mut().retain(|c| {
        let policy = is_policy(c);
        n_moved += usize::from(policy);
        policy
    });
    let _ = children;
    ((n_keep > 0).then_some(keep), (n_moved > 0).then_some(moved))
}

/// The string arguments of a node, in order.
fn string_args(n: &KdlNode) -> impl Iterator<Item = &str> {
    n.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .filter_map(|e| e.value().as_string())
}

/// Move the built-in applet ids out of `bar { tray { pinned …; hidden … } }`
/// into `bar { widgets { order … } }` (ADR 0065). Returns whether anything
/// changed.
///
/// - A pinned built-in keeps its place relative to the other pinned ones and
///   leads the built-ins; one neither pinned nor hidden (it sat in the
///   overflow drawer) follows in the default order. The widgets have no
///   drawer, so "reachable" becomes "drawn".
/// - A hidden built-in is left out of `order`, which is how a widget is not
///   drawn. Hidden wins over pinned, as it did in the tray.
/// - Everything else keeps its place in the default order (`now-playing`
///   before the built-ins, `tray` and `clock` after).
/// - An existing `widgets { order … }` is the human's newer word and is kept;
///   the legacy ids are only stripped from the tray lists.
/// - An emptied `pinned` stays (empty still means "pin no app"); an emptied
///   `hidden` goes (empty is its default).
fn migrate_widgets(doc: &mut KdlDocument) -> bool {
    let legacy = |s: &str| LEGACY_TRAY_BUILTINS.contains(&s);
    let mut pinned: Vec<String> = Vec::new();
    let mut hidden: Vec<String> = Vec::new();
    let mut has_order = false;
    let mut target = None;
    for (i, bar) in doc.nodes().iter().enumerate() {
        if bar.name().value() != "bar" {
            continue;
        }
        for child in bar.children().map(|c| c.nodes()).unwrap_or_default() {
            match child.name().value() {
                "widgets" => {
                    has_order |= child
                        .children()
                        .is_some_and(|c| c.nodes().iter().any(|n| n.name().value() == "order"));
                }
                "tray" => {
                    for list in child.children().map(|c| c.nodes()).unwrap_or_default() {
                        let into = match list.name().value() {
                            "pinned" => &mut pinned,
                            "hidden" => &mut hidden,
                            _ => continue,
                        };
                        for id in string_args(list).filter(|s| legacy(s)) {
                            target.get_or_insert(i);
                            if !into.iter().any(|x| x == id) {
                                into.push(id.to_owned());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let Some(target) = target else { return false };

    // Strip the legacy ids from every tray list.
    for bar in doc.nodes_mut().iter_mut().filter(|n| n.name().value() == "bar") {
        let Some(children) = bar.children_mut().as_mut() else {
            continue;
        };
        for tray in children
            .nodes_mut()
            .iter_mut()
            .filter(|n| n.name().value() == "tray")
        {
            let Some(lists) = tray.children_mut().as_mut() else {
                continue;
            };
            for list in lists.nodes_mut() {
                if matches!(list.name().value(), "pinned" | "hidden") {
                    list.entries_mut()
                        .retain(|e| !e.value().as_string().is_some_and(legacy));
                }
            }
            lists
                .nodes_mut()
                .retain(|n| n.name().value() != "hidden" || !n.entries().is_empty());
        }
    }
    if has_order {
        return true;
    }

    // The default order, with its run of built-ins replaced by the pinned ones
    // first and the unpinned ones after, then the hidden ones dropped.
    let mut order: Vec<&str> = Vec::new();
    let mut placed = false;
    for id in BAR_WIDGET_DEFAULT_ORDER.iter().copied() {
        if !legacy(id) {
            order.push(id);
        } else if !placed {
            placed = true;
            order.extend(pinned.iter().map(String::as_str));
            order.extend(
                BAR_WIDGET_DEFAULT_ORDER
                    .iter()
                    .copied()
                    .filter(|b| legacy(b) && !pinned.iter().any(|p| p == b)),
            );
        }
    }
    order.retain(|id| !hidden.iter().any(|h| h == id));
    let list: Vec<String> = order.iter().map(|id| format!("{id:?}")).collect();

    // Spliced as parsed text so the new node carries ordinary formatting and
    // the rest of the human's `bar` block keeps theirs, comments included.
    // The target `bar` has a `tray` child, so it has a children block.
    let children = doc.nodes_mut()[target].ensure_children();
    let (into, snippet) = match children
        .nodes_mut()
        .iter_mut()
        .position(|n| n.name().value() == "widgets")
    {
        Some(w) => (
            children.nodes_mut()[w].ensure_children(),
            format!("        order {}\n", list.join(" ")),
        ),
        None => (
            children,
            format!("    widgets {{\n        order {}\n    }}\n", list.join(" ")),
        ),
    };
    let snippet: KdlDocument = snippet.parse().expect("generated node parses");
    into.nodes_mut().extend(snippet.nodes().iter().cloned());
    true
}

/// Rewrite the legacy `decoration { blur { enabled #true|#false } }` as
/// `mode "blur"|"off"` (COMP-02 §9 blur modes). The node is edited in place so
/// its comments and position survive; an existing `mode` is the human's newer
/// word and wins, and the stale `enabled` is just dropped. Returns whether
/// anything changed.
fn migrate_blur_mode(doc: &mut KdlDocument) -> bool {
    let mut changed = false;
    for deco in doc
        .nodes_mut()
        .iter_mut()
        .filter(|n| n.name().value() == "decoration")
    {
        let Some(children) = deco.children_mut().as_mut() else {
            continue;
        };
        for blur in children
            .nodes_mut()
            .iter_mut()
            .filter(|n| n.name().value() == "blur")
        {
            let Some(keys) = blur.children_mut().as_mut() else {
                continue;
            };
            let has_mode = keys.nodes().iter().any(|n| n.name().value() == "mode");
            let mut drop = false;
            for node in keys
                .nodes_mut()
                .iter_mut()
                .filter(|n| n.name().value() == "enabled")
            {
                changed = true;
                if has_mode {
                    drop = true;
                    continue;
                }
                // A bare `enabled` meant true, as the parser reads it.
                let on = node
                    .entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .and_then(|e| e.value().as_bool())
                    .unwrap_or(true);
                node.set_name("mode");
                node.entries_mut().clear();
                node.push(kdl::KdlEntry::new(if on { "blur" } else { "off" }));
            }
            if drop {
                keys.nodes_mut().retain(|n| n.name().value() != "enabled");
            }
        }
    }
    changed
}

/// Bare `enabled` means true, as the parser reads it.
fn flag_of(n: &KdlNode) -> Option<bool> {
    match n.entries().iter().find(|e| e.name().is_none()) {
        None => Some(true),
        Some(e) => e.value().as_bool(),
    }
}

/// The legacy `duration=` grammar: an integer of ms, or `"150ms"`, `"1s"`.
fn parse_duration_ms(v: &kdl::KdlValue) -> Option<u32> {
    if let Some(i) = v.as_integer() {
        return u32::try_from(i).ok();
    }
    let s = v.as_string()?.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else {
        (s, 1)
    };
    num.trim().parse::<u32>().ok()?.checked_mul(mult)
}

/// A legacy `animation "<name>" duration=… curve=…` as the event block it
/// stands for: `(event, style, ms, curve)`. `None` for a node the parser
/// refuses, which is left where it is for the human to see in the errors.
fn legacy_animation(n: &KdlNode) -> Option<(&'static str, &'static str, u32, String)> {
    let name = string_args(n).next()?;
    let &(_, ev, style) = LEGACY_ANIMATION_MAP.iter().find(|(l, _, _)| *l == name)?;
    let mut ms = ANIMATION_DEFAULT_MS;
    let mut curve = ANIMATION_DEFAULT_CURVE.to_owned();
    for e in n.entries() {
        match e.name().map(|k| k.value()) {
            None => {}
            Some("duration") => ms = parse_duration_ms(e.value()).filter(|ms| *ms <= ANIMATION_MAX_MS)?,
            Some("curve") => {
                curve = e
                    .value()
                    .as_string()
                    .filter(|c| EASING_CURVES.contains(c))?
                    .to_owned()
            }
            Some(_) => return None,
        }
    }
    Some((ev, style, ms, curve))
}

/// An event block, as parsed text: spliced rather than built so it carries
/// ordinary formatting.
fn event_block(ev: &str, style: Option<&str>, ms: Option<u32>, curve: Option<&str>) -> KdlNode {
    let doc: KdlDocument = event_text(ev, style, ms, curve)
        .parse()
        .expect("generated node parses");
    doc.nodes()[0].clone()
}

fn event_text(ev: &str, style: Option<&str>, ms: Option<u32>, curve: Option<&str>) -> String {
    let mut text = format!("    {ev} {{\n");
    if let Some(s) = style {
        text.push_str(&format!("        style {s:?}\n"));
    }
    if let Some(ms) = ms {
        text.push_str(&format!("        duration-ms {ms}\n"));
    }
    if let Some(c) = curve {
        text.push_str(&format!("        curve {c:?}\n"));
    }
    text.push_str("    }\n");
    text
}

/// Rewrite the legacy animation forms (COMP-02 §9 animation presets) and
/// carry `bar.motion` over to `animations.bar-layout`. Returns whether
/// anything changed. Per `animations` block, as the parser reads them:
///
/// - `enabled #false` becomes `preset "off"`, unless the block already names
///   a preset; its `animation` nodes were inert and go.
/// - `enabled #true` goes, and each valid `animation` node becomes the event
///   block it stood for (`animation "windows"` -> `window-move { style
///   "glide"; … }`, old defaults filled in), unless that event already has a
///   block. The last node for an event wins, as it did.
/// - `animation` nodes without `enabled #true` were inert and go.
/// - An invalid `animation` node is left as is: the loader reports it.
///
/// `bar { motion { … } }` stays (the taskbar still reads it); if the human
/// set any of it and no `bar-layout` block exists, one is added saying the
/// same thing: `enabled` as style `glide`/`none` (unset meant on), and
/// `duration-ms` and `curve` only where they were written.
fn migrate_animations(doc: &mut KdlDocument) -> bool {
    let mut changed = false;
    let mut has_bar_layout = false;
    for block in doc
        .nodes_mut()
        .iter_mut()
        .filter(|n| n.name().value() == "animations")
    {
        let Some(kids) = block.children_mut().as_mut() else {
            continue;
        };
        let names = |kids: &KdlDocument, name: &str| kids.nodes().iter().any(|n| n.name().value() == name);
        let has_preset = names(kids, "preset");
        let enabled = kids
            .nodes()
            .iter()
            .rev()
            .filter(|n| n.name().value() == "enabled")
            .filter_map(flag_of)
            .next();
        let mut legacy: Vec<(usize, &'static str, &'static str, u32, String)> = Vec::new();
        let mut inert: Vec<usize> = Vec::new();
        for (i, n) in kids.nodes().iter().enumerate() {
            if n.name().value() != "animation" {
                continue;
            }
            if let Some((ev, style, ms, curve)) = legacy_animation(n) {
                if enabled == Some(true) && !names(kids, ev) {
                    legacy.retain(|l| l.1 != ev);
                    legacy.push((i, ev, style, ms, curve));
                } else {
                    inert.push(i);
                }
            }
        }
        let has_enabled = names(kids, "enabled");
        if !has_enabled && legacy.is_empty() && inert.is_empty() {
            has_bar_layout |= names(kids, "bar-layout");
            continue;
        }
        changed = true;

        // Replace in place, keeping each node's leading comments.
        let mut drop: Vec<usize> = inert;
        for (i, n) in kids.nodes().iter().enumerate() {
            if n.name().value() == "animation"
                && enabled == Some(true)
                && legacy_animation(n).is_some()
                && !legacy.iter().any(|l| l.0 == i)
            {
                drop.push(i);
            }
        }
        for (i, ev, style, ms, curve) in &legacy {
            let mut node = event_block(ev, Some(style), Some(*ms), Some(curve));
            if let (Some(old), Some(new)) = (kids.nodes()[*i].format().cloned(), node.format_mut()) {
                new.leading = old.leading;
            }
            kids.nodes_mut()[*i] = node;
        }
        let mut off_placed = false;
        for (i, n) in kids.nodes_mut().iter_mut().enumerate() {
            if n.name().value() != "enabled" {
                continue;
            }
            if enabled == Some(false) && !has_preset && !off_placed {
                off_placed = true;
                n.set_name("preset");
                n.entries_mut().clear();
                n.push(kdl::KdlEntry::new("off"));
            } else {
                drop.push(i);
            }
        }
        // The newline after `{` is the first child's leading trivia; when that
        // child goes, the next one inherits it.
        let first_break = drop.contains(&0)
            && kids
                .nodes()
                .first()
                .and_then(|n| n.format())
                .is_some_and(|f| f.leading.starts_with('\n'));
        let mut i = 0;
        kids.nodes_mut().retain(|_| {
            i += 1;
            !drop.contains(&(i - 1))
        });
        if first_break {
            if let Some(f) = kids.nodes_mut().first_mut().and_then(|n| n.format_mut()) {
                if !f.leading.starts_with('\n') {
                    f.leading.insert(0, '\n');
                }
            }
        }
        has_bar_layout |= names(kids, "bar-layout");
    }

    // `bar { motion { … } }`: the last value of each key wins, as parsed.
    let (mut on, mut ms, mut curve) = (None, None, None);
    let mut any = false;
    for bar in doc.nodes().iter().filter(|n| n.name().value() == "bar") {
        for motion in bar
            .children()
            .map(|c| c.nodes())
            .unwrap_or_default()
            .iter()
            .filter(|n| n.name().value() == "motion")
        {
            for k in motion.children().map(|c| c.nodes()).unwrap_or_default() {
                let v = k.entries().iter().find(|e| e.name().is_none()).map(|e| e.value());
                match k.name().value() {
                    "enabled" => {
                        if let Some(b) = flag_of(k) {
                            on = Some(b);
                            any = true;
                        }
                    }
                    "duration-ms" => {
                        if let Some(d) = v
                            .and_then(kdl::KdlValue::as_integer)
                            .filter(|d| (0..=BAR_MOTION_MAX_MS).contains(d))
                        {
                            ms = Some(d as u32);
                            any = true;
                        }
                    }
                    "curve" => {
                        if let Some(c) = v
                            .and_then(kdl::KdlValue::as_string)
                            .filter(|c| ANIMATION_CURVES.contains(c))
                        {
                            curve = Some(c.to_owned());
                            any = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if any && !has_bar_layout {
        let style = if on.unwrap_or(true) { "glide" } else { "none" };
        match doc
            .nodes_mut()
            .iter_mut()
            .rev()
            .find(|n| n.name().value() == "animations")
        {
            Some(block) => block.ensure_children().nodes_mut().push(event_block(
                "bar-layout",
                Some(style),
                ms,
                curve.as_deref(),
            )),
            None => {
                let text = format!(
                    "animations {{\n{}}}\n",
                    event_text("bar-layout", Some(style), ms, curve.as_deref())
                );
                let snippet: KdlDocument = text.parse().expect("generated node parses");
                doc.nodes_mut().extend(snippet.nodes().iter().cloned());
            }
        }
        changed = true;
    }
    changed
}

/// Returns `(abyss.kdl, policy.kdl, moved)` as text; `moved` is whether any
/// node went to the policy side.
fn split(doc: &KdlDocument, existing_policy: &KdlDocument) -> (String, String, bool) {
    let mut abyss = KdlDocument::new();
    let mut policy = existing_policy.clone();
    let mut any_moved = false;
    for node in doc.nodes() {
        let name = node.name().value().to_string();
        let (keep, moved) = if name == "windowrule" {
            split_node(node, rule_action_is_policy)
        } else {
            split_node(node, |c| child_is_policy(&name, c))
        };
        if let Some(k) = keep {
            abyss.nodes_mut().push(k);
        }
        if let Some(m) = moved {
            policy.nodes_mut().push(m);
            any_moved = true;
        }
    }
    (abyss.to_string(), policy.to_string(), any_moved)
}

pub fn run(dry_run: bool) -> Result<String, String> {
    let dir = config_dir()?;
    let abyss_path = dir.join("abyss.kdl");
    let policy_path = dir.join("policy.kdl");

    let abyss_text =
        std::fs::read_to_string(&abyss_path).map_err(|e| format!("{}: {e}", abyss_path.display()))?;
    let policy_text = std::fs::read_to_string(&policy_path).unwrap_or_default();

    let mut doc: KdlDocument = abyss_text
        .parse()
        .map_err(|e| format!("{}: {e}", abyss_path.display()))?;
    let existing: KdlDocument = policy_text
        .parse()
        .map_err(|e| format!("{}: {e}", policy_path.display()))?;

    let widgets = migrate_widgets(&mut doc);
    let blur = migrate_blur_mode(&mut doc);
    let anims = migrate_animations(&mut doc);
    let (split_abyss, mut new_policy, moved) = split(&doc, &existing);
    if !widgets && !blur && !anims && !moved {
        return Ok(format!(
            "{}: nothing to migrate — no policy settings, built-in tray ids, blur `enabled`, \
             legacy animation forms or un-migrated bar.motion found\n",
            abyss_path.display()
        ));
    }
    // With nothing moved, the document is written back whole so its own
    // leading and trailing trivia survive too.
    let new_abyss = if moved { split_abyss } else { doc.to_string() };
    if policy_text.trim().is_empty() {
        new_policy = format!("{POLICY_HEADER}{new_policy}");
    }

    // Refuse to commit anything we cannot read back. This is the check that
    // makes the verb safe to run blind.
    new_abyss.parse::<KdlDocument>().map_err(|e| {
        format!(
            "refusing to write {}: it would not re-parse: {e}",
            abyss_path.display()
        )
    })?;
    if moved {
        new_policy.parse::<KdlDocument>().map_err(|e| {
            format!(
                "refusing to write {}: it would not re-parse: {e}",
                policy_path.display()
            )
        })?;
    }

    if dry_run {
        let mut out = format!("--- {} ---\n{new_abyss}\n", abyss_path.display());
        if moved {
            out.push_str(&format!("--- {} ---\n{new_policy}", policy_path.display()));
        }
        return Ok(out);
    }

    // Back up first: `policy.kdl` may already exist with content a human
    // wrote, and this appends to it.
    backup(&abyss_path, &abyss_text)?;
    if moved && !policy_text.is_empty() {
        backup(&policy_path, &policy_text)?;
    }
    write(&abyss_path, &new_abyss)?;
    if !moved {
        let what = [
            (widgets, "built-in tray ids -> bar.widgets.order"),
            (blur, "decoration.blur.enabled -> decoration.blur.mode"),
            (anims, "legacy animations -> presets and event blocks"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, w)| *w)
        .collect::<Vec<_>>()
        .join(", ");
        return Ok(format!(
            "migrated: {} {what} (original saved as .bak)\n",
            abyss_path.display()
        ));
    }
    write(&policy_path, &new_policy)?;
    Ok(format!(
        "migrated: {} -> {} (originals saved as .bak)\n",
        abyss_path.display(),
        policy_path.display()
    ))
}

fn backup(path: &std::path::Path, text: &str) -> Result<(), String> {
    let bak = path.with_extension("kdl.bak");
    std::fs::write(&bak, text).map_err(|e| format!("{}: {e}", bak.display()))
}

fn write(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("kdl.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> KdlDocument {
        s.parse().expect("valid kdl")
    }

    #[test]
    fn policy_children_move_and_the_rest_stays() {
        let doc = parse(
            "general {\n    gaps-in 5\n}\ncapture {\n    allow \"obs\"\n}\nmisc {\n    scripted-input #false\n    vrr #true\n}\n",
        );
        let (abyss, policy, _) = split(&doc, &KdlDocument::new());
        assert!(abyss.contains("gaps-in"), "{abyss}");
        assert!(abyss.contains("vrr"), "{abyss}");
        assert!(!abyss.contains("scripted-input"), "{abyss}");
        assert!(!abyss.contains("capture"), "{abyss}");
        assert!(policy.contains("scripted-input"), "{policy}");
        assert!(policy.contains("allow \"obs\""), "{policy}");
        // `misc` survives on both sides because both halves are non-empty.
        assert!(policy.contains("misc"), "{policy}");
    }

    #[test]
    fn a_mixed_windowrule_is_split_in_two() {
        let doc = parse("windowrule \"app-id=signal\" {\n    float #true\n    sensitivity \"secret\"\n}\n");
        let (abyss, policy, _) = split(&doc, &KdlDocument::new());
        assert!(abyss.contains("float"), "{abyss}");
        assert!(!abyss.contains("sensitivity"), "{abyss}");
        assert!(policy.contains("sensitivity"), "{policy}");
        assert!(policy.contains("app-id=signal"), "{policy}");
        assert!(!policy.contains("float"), "{policy}");
    }

    fn widgets(text: &str) -> (bool, String) {
        let mut doc = parse(text);
        let changed = migrate_widgets(&mut doc);
        let out = doc.to_string();
        out.parse::<KdlDocument>().expect("migrated text re-parses");
        (changed, out)
    }

    #[test]
    fn pinned_built_ins_lead_and_hidden_ones_are_dropped() {
        let (changed, out) = widgets(
            "bar {\n    // my tray\n    tray {\n        pinned \"battery\" \"org.kde.x\" \"volume\"\n        hidden \"bluetooth\"\n    }\n}\n",
        );
        assert!(changed);
        assert_eq!(
            out,
            "bar {\n    // my tray\n    tray {\n        pinned \"org.kde.x\"\n    }\n    widgets {\n        \
             order \"now-playing\" \"battery\" \"volume\" \"network\" \"tray\" \"clock\"\n    }\n}\n"
        );
    }

    #[test]
    fn an_emptied_pinned_stays_and_unset_pinned_keeps_the_default_order() {
        let (_, out) = widgets("bar {\n    tray {\n        pinned \"network\"\n    }\n}\n");
        assert!(out.contains("        pinned\n"), "{out}");
        assert!(
            out.contains(
                "order \"now-playing\" \"network\" \"volume\" \"bluetooth\" \"battery\" \"tray\" \"clock\""
            ),
            "{out}"
        );
        let (_, out) = widgets("bar {\n    tray {\n        hidden \"volume\"\n    }\n}\n");
        assert!(!out.contains("hidden"), "{out}");
        assert!(
            out.contains("order \"now-playing\" \"network\" \"bluetooth\" \"battery\" \"tray\" \"clock\""),
            "{out}"
        );
    }

    #[test]
    fn an_existing_order_is_kept_and_only_the_tray_is_cleaned() {
        let (changed, out) = widgets(
            "bar {\n    widgets {\n        order \"clock\"\n    }\n    tray {\n        hidden \"battery\" \"org.x\"\n    }\n}\n",
        );
        assert!(changed);
        assert_eq!(
            out,
            "bar {\n    widgets {\n        order \"clock\"\n    }\n    tray {\n        hidden \"org.x\"\n    }\n}\n"
        );
        // A `widgets` block without an order gains one.
        let (_, out) = widgets(
            "bar {\n    widgets {\n        volume {\n            step 2\n        }\n    }\n    tray {\n        hidden \"battery\"\n    }\n}\n",
        );
        assert!(out.contains("        }\n        order \"now-playing\""), "{out}");
        assert_eq!(out.matches("widgets").count(), 1, "{out}");
    }

    #[test]
    fn nothing_to_do_without_built_in_ids() {
        let text = "bar {\n    tray {\n        pinned \"org.kde.x\"\n    }\n}\n";
        assert_eq!(widgets(text), (false, text.to_string()));
        assert!(!widgets("general {\n    gaps-in 5\n}\n").0);
    }

    fn blur(text: &str) -> (bool, String) {
        let mut doc = parse(text);
        let changed = migrate_blur_mode(&mut doc);
        let out = doc.to_string();
        out.parse::<KdlDocument>().expect("migrated text re-parses");
        (changed, out)
    }

    #[test]
    fn blur_enabled_becomes_a_mode() {
        let (changed, out) = blur(
            "decoration {\n    blur {\n        // mine\n        enabled #false\n        size 4\n    }\n}\n",
        );
        assert!(changed);
        assert_eq!(
            out,
            "decoration {\n    blur {\n        // mine\n        mode off\n        size 4\n    }\n}\n"
        );
        let (_, out) = blur("decoration {\n    blur {\n        enabled #true\n    }\n}\n");
        assert!(out.contains("mode blur\n"), "{out}");
        let (_, out) = blur("decoration {\n    blur {\n        enabled\n    }\n}\n");
        assert!(out.contains("mode blur\n"), "{out}");
    }

    #[test]
    fn an_existing_blur_mode_wins_over_enabled() {
        let (changed, out) =
            blur("decoration {\n    blur {\n        mode glass\n        enabled #false\n    }\n}\n");
        assert!(changed);
        assert_eq!(out, "decoration {\n    blur {\n        mode glass\n    }\n}\n");
        assert!(!blur("decoration {\n    blur {\n        mode frost\n    }\n}\n").0);
    }

    fn anims(text: &str) -> (bool, String) {
        let mut doc = parse(text);
        let changed = migrate_animations(&mut doc);
        let out = doc.to_string();
        out.parse::<KdlDocument>().expect("migrated text re-parses");
        (changed, out)
    }

    #[test]
    fn animations_disabled_becomes_preset_off() {
        let (changed, out) = anims(
            "animations {\n    // no motion\n    enabled #false\n    animation \"windows\" duration=\"200ms\"\n}\n",
        );
        assert!(changed);
        assert_eq!(out, "animations {\n    // no motion\n    preset off\n}\n");
        // An existing preset is the newer word: `enabled` just goes.
        let (_, out) = anims("animations {\n    preset \"smooth\"\n    enabled #false\n}\n");
        assert_eq!(out, "animations {\n    preset \"smooth\"\n}\n");
    }

    #[test]
    fn enabled_legacy_animations_become_event_blocks() {
        let (changed, out) = anims(
            "animations {\n    enabled #true\n    // slide\n    animation \"workspaces\" duration=\"1s\" curve=\"linear\"\n    \
             animation \"fade\"\n    animation \"border\" duration=90\n    animation \"border\" duration=40\n}\n",
        );
        assert!(changed);
        assert_eq!(
            out,
            "animations {\n    // slide\n    workspace-switch {\n        style \"slide\"\n        duration-ms 1000\n        \
             curve \"linear\"\n    }\n    window-open {\n        style \"fade\"\n        duration-ms 150\n        \
             curve \"ease-out\"\n    }\n    focus {\n        style \"crossfade\"\n        duration-ms 40\n        \
             curve \"ease-out\"\n    }\n}\n"
        );
    }

    #[test]
    fn an_existing_event_block_and_bad_nodes_are_kept() {
        let (_, out) = anims(
            "animations {\n    enabled #true\n    focus {\n        duration-ms 10\n    }\n    animation \"border\"\n    \
             animation \"windows\" curve=\"spring\"\n}\n",
        );
        assert_eq!(
            out,
            "animations {\n    focus {\n        duration-ms 10\n    }\n    animation \"windows\" curve=\"spring\"\n}\n"
        );
        // Legacy nodes without `enabled #true` were inert and go.
        let (changed, out) = anims("animations {\n    preset \"subtle\"\n    animation \"fade\"\n}\n");
        assert!(changed);
        assert_eq!(out, "animations {\n    preset \"subtle\"\n}\n");
    }

    #[test]
    fn bar_motion_is_carried_to_bar_layout_and_kept() {
        let (changed, out) = anims("bar {\n    motion {\n        duration-ms 300\n    }\n}\n");
        assert!(changed);
        assert_eq!(
            out,
            "bar {\n    motion {\n        duration-ms 300\n    }\n}\nanimations {\n    bar-layout {\n        \
             style \"glide\"\n        duration-ms 300\n    }\n}\n"
        );
        let (_, out) = anims(
            "animations {\n    preset \"smooth\"\n}\nbar {\n    motion {\n        enabled #false\n        curve \"bounce\"\n    }\n}\n",
        );
        assert!(
            out.starts_with(
                "animations {\n    preset \"smooth\"\n    bar-layout {\n        style \"none\"\n        curve \"bounce\"\n    }\n}\n"
            ),
            "{out}"
        );
        // Once a bar-layout block exists, the migration is done.
        let text = "animations {\n    bar-layout {\n        style \"none\"\n    }\n}\nbar {\n    motion {\n        enabled #true\n    }\n}\n";
        assert_eq!(anims(text), (false, text.to_string()));
        assert!(!anims("general {\n    gaps-in 5\n}\n").0);
    }

    #[test]
    fn a_document_with_no_policy_keys_produces_no_policy_file() {
        let doc = parse("general {\n    gaps-in 5\n}\n");
        let (_, policy, moved) = split(&doc, &KdlDocument::new());
        assert!(!moved);
        assert!(policy.trim().is_empty(), "{policy}");
    }
}
