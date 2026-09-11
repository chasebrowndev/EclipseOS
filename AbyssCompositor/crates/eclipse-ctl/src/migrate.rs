// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-ctl config migrate` — split a legacy single `abyss.kdl` into the
//! two files COMP-13 §1.3 expects: `abyss.kdl` and `policy.kdl`.
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
];

/// `windowrule` actions the schema marks `Owner::Policy`
/// (`abyss::config::schema::RULE_ACTIONS`). A rule carrying both kinds is
/// split in two, one node per file, keeping the same match criteria.
const POLICY_RULE_ACTIONS: &[&str] = &["sensitivity", "app-trust", "seat-compat", "no-agent"];

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

/// Returns `(abyss.kdl, policy.kdl)` as text.
fn split(doc: &KdlDocument, existing_policy: &KdlDocument) -> (String, String) {
    let mut abyss = KdlDocument::new();
    let mut policy = existing_policy.clone();
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
        }
    }
    (abyss.to_string(), policy.to_string())
}

pub fn run(dry_run: bool) -> Result<String, String> {
    let dir = config_dir()?;
    let abyss_path = dir.join("abyss.kdl");
    let policy_path = dir.join("policy.kdl");

    let abyss_text =
        std::fs::read_to_string(&abyss_path).map_err(|e| format!("{}: {e}", abyss_path.display()))?;
    let policy_text = std::fs::read_to_string(&policy_path).unwrap_or_default();

    let doc: KdlDocument = abyss_text
        .parse()
        .map_err(|e| format!("{}: {e}", abyss_path.display()))?;
    let existing: KdlDocument = policy_text
        .parse()
        .map_err(|e| format!("{}: {e}", policy_path.display()))?;

    let (new_abyss, mut new_policy) = split(&doc, &existing);
    if new_policy.trim().is_empty() {
        return Ok(format!(
            "{}: nothing to migrate — no policy settings found\n",
            abyss_path.display()
        ));
    }
    if policy_text.trim().is_empty() {
        new_policy = format!("{POLICY_HEADER}{new_policy}");
    }

    // Refuse to commit anything we cannot read back. This is the check that
    // makes the verb safe to run blind.
    for (path, text) in [(&abyss_path, &new_abyss), (&policy_path, &new_policy)] {
        text.parse::<KdlDocument>()
            .map_err(|e| format!("refusing to write {}: it would not re-parse: {e}", path.display()))?;
    }

    if dry_run {
        return Ok(format!(
            "--- {} ---\n{new_abyss}\n--- {} ---\n{new_policy}",
            abyss_path.display(),
            policy_path.display()
        ));
    }

    // Back up both sides first: `policy.kdl` may already exist with content a
    // human wrote, and this appends to it.
    backup(&abyss_path, &abyss_text)?;
    if !policy_text.is_empty() {
        backup(&policy_path, &policy_text)?;
    }
    write(&abyss_path, &new_abyss)?;
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
        let (abyss, policy) = split(&doc, &KdlDocument::new());
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
        let (abyss, policy) = split(&doc, &KdlDocument::new());
        assert!(abyss.contains("float"), "{abyss}");
        assert!(!abyss.contains("sensitivity"), "{abyss}");
        assert!(policy.contains("sensitivity"), "{policy}");
        assert!(policy.contains("app-id=signal"), "{policy}");
        assert!(!policy.contains("float"), "{policy}");
    }

    #[test]
    fn a_document_with_no_policy_keys_produces_no_policy_file() {
        let doc = parse("general {\n    gaps-in 5\n}\n");
        let (_, policy) = split(&doc, &KdlDocument::new());
        assert!(policy.trim().is_empty(), "{policy}");
    }
}
