// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-ctl config migrate` — split a legacy single `abyss.kdl` into the
//! two files COMP-13 §1.3 expects: `abyss.kdl` and `policy.kdl`. It also
//! moves the built-in applet ids out of `bar.tray` into `bar.widgets.order`
//! (ADR 0065; see [`migrate_widgets`]).
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
/// (`abyss::config::schema::TABLE`). Mirrored here so this CLI does not link
/// the compositor; `tests/schema_drift.rs` fails if the two ever diverge.
const POLICY_KEYS: &[&str] = &[
    "misc.scripted-input",
    "clipboard.data-control-allow",
    "capture.allow",
    "capture.redact-app-id",
    "capture.hide-layer",
];

/// `windowrule` actions the schema marks `Owner::Policy`
/// (`abyss::config::schema::RULE_ACTIONS`). A rule carrying both kinds is
/// split in two, one node per file, keeping the same match criteria.
const POLICY_RULE_ACTIONS: &[&str] = &["sensitivity", "app-trust", "seat-compat", "no-agent"];

/// Built-in applet ids `bar.tray.pinned`/`hidden` carried before they became
/// taskbar widgets (ADR 0065). Mirrors `abyss::config::schema::LEGACY_TRAY_BUILTINS`;
/// `tests/schema_drift.rs` pins both copies.
const LEGACY_TRAY_BUILTINS: &[&str] = &["network", "bluetooth", "battery", "volume"];

/// `abyss::config::schema::BAR_WIDGET_DEFAULT_ORDER`. The migrated order is
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

const POLICY_HEADER: &str = "\
// policy.kdl — the security surface (COMP-13 §1.3).
//
// Split out of abyss.kdl by `eclipse-ctl config migrate`. The control socket
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
    let (split_abyss, mut new_policy, moved) = split(&doc, &existing);
    if !widgets && !moved {
        return Ok(format!(
            "{}: nothing to migrate — no policy settings or built-in tray ids found\n",
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
        return Ok(format!(
            "migrated: {} built-in tray ids -> bar.widgets.order (original saved as .bak)\n",
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

    #[test]
    fn a_document_with_no_policy_keys_produces_no_policy_file() {
        let doc = parse("general {\n    gaps-in 5\n}\n");
        let (_, policy, moved) = split(&doc, &KdlDocument::new());
        assert!(!moved);
        assert!(policy.trim().is_empty(), "{policy}");
    }
}
