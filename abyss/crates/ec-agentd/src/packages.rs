// SPDX-License-Identifier: AGPL-3.0-only
//! Installed agent packages (A-07 §2): what `list_packages` shows and what the
//! launcher runs.
//!
//! Layout `<root>/<id>/<version>/manifest.kdl`; roots in order are
//! `~/.local/share/eclipse/agents/` (publisher "local") and
//! `/usr/share/eclipse/agents/` (publisher "eclipse"). The publisher comes
//! from the root, never from the manifest's own claim.
//!
//! TODO(ec-manifest): this is a minimal read of `agent { id name version
//! entrypoint }`. Replace it with the shared A-07 parser when it lands.

use std::path::{Component, Path, PathBuf};

use kdl::KdlDocument;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

impl Package {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "publisher": self.publisher, "version": self.version})
    }
}

/// A package id or version used as a path component: no separators, no
/// leading dot, a plain character set.
pub fn safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}

fn agent_children(text: &str) -> Option<KdlDocument> {
    let doc: KdlDocument = text.parse().ok()?;
    doc.get("agent")?.children().cloned()
}

fn arg(doc: &KdlDocument, name: &str) -> Option<String> {
    doc.get_arg(name)?.as_string().map(str::to_owned)
}

fn read_manifest(dir: &Path) -> Option<KdlDocument> {
    agent_children(&std::fs::read_to_string(dir.join("manifest.kdl")).ok()?)
}

pub fn list(roots: &[(PathBuf, String)]) -> Vec<Package> {
    let mut out = Vec::new();
    for (root, publisher) in roots {
        let Ok(ids) = std::fs::read_dir(root) else {
            continue;
        };
        for id in ids.flatten() {
            let id_name = id.file_name().to_string_lossy().into_owned();
            if !safe_component(&id_name) {
                continue;
            }
            let Ok(versions) = std::fs::read_dir(id.path()) else {
                continue;
            };
            for ver in versions.flatten() {
                let ver_name = ver.file_name().to_string_lossy().into_owned();
                if !safe_component(&ver_name) {
                    continue;
                }
                let Some(m) = read_manifest(&ver.path()) else {
                    continue;
                };
                // The directory names the package; a manifest that disagrees
                // is not listed.
                if arg(&m, "id").as_deref() != Some(id_name.as_str())
                    || arg(&m, "version").as_deref() != Some(ver_name.as_str())
                {
                    continue;
                }
                out.push(Package {
                    name: arg(&m, "name").unwrap_or_else(|| id_name.clone()),
                    id: id_name.clone(),
                    publisher: publisher.clone(),
                    version: ver_name,
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.id, &a.version, &a.publisher).cmp(&(&b.id, &b.version, &b.publisher)));
    out
}

/// `<root>/<id>/<version>/` for the first root that has it.
pub fn find_dir(roots: &[(PathBuf, String)], id: &str, version: &str) -> Option<PathBuf> {
    if !safe_component(id) || !safe_component(version) {
        return None;
    }
    roots
        .iter()
        .map(|(r, _)| r.join(id).join(version))
        .find(|d| d.join("manifest.kdl").is_file())
}

/// The manifest's `entrypoint`, a list of words. A first word containing `/`
/// is a path inside the package: refused when absolute or containing `..`
/// (A-07 §5). A bare first word is a runtime command.
pub fn entrypoint(pkg_dir: &Path) -> Result<Vec<String>, String> {
    let m = read_manifest(pkg_dir).ok_or("no readable manifest")?;
    let node = m.get("entrypoint").ok_or("manifest has no entrypoint")?;
    let words: Vec<String> = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .filter_map(|e| e.value().as_string().map(str::to_owned))
        .collect();
    let first = words.first().ok_or("empty entrypoint")?;
    if first.is_empty() || words.iter().any(|w| w.contains('\0')) {
        return Err("bad entrypoint".into());
    }
    if first.contains('/') {
        let p = Path::new(first);
        if p.is_absolute()
            || p.components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err("entrypoint escapes the package".into());
        }
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(dir: &Path, id: &str, ver: &str, entry: &str) {
        let d = dir.join(id).join(ver);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("manifest.kdl"),
            format!("agent {{\n  id \"{id}\"\n  name \"Name {id}\"\n  version \"{ver}\"\n  publisher \"evil\"\n  entrypoint {entry}\n  capabilities {{ }}\n}}\n"),
        )
        .unwrap();
    }

    #[test]
    fn lists_with_root_publisher_and_checks_entrypoint() {
        let tmp = crate::scratch_dir("pkgs");
        let local = tmp.join("local");
        let sys = tmp.join("sys");
        manifest(&local, "triage", "0.3.1", "\"bin/run\" \"--x\"");
        manifest(&sys, "ref", "1.0.0", "\"python\" \"-m\" \"ref\"");
        let roots = vec![(local.clone(), "local".to_owned()), (sys, "eclipse".to_owned())];
        let l = list(&roots);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].publisher, "eclipse");
        assert_eq!(l[1].publisher, "local");
        assert_eq!(l[1].name, "Name triage");
        let d = find_dir(&roots, "triage", "0.3.1").unwrap();
        assert_eq!(entrypoint(&d).unwrap(), ["bin/run", "--x"]);
        assert!(find_dir(&roots, "../x", "1").is_none());
    }

    #[test]
    fn refuses_escaping_entrypoints() {
        let tmp = crate::scratch_dir("pkgs-bad");
        manifest(&tmp, "a", "1", "\"/bin/sh\"");
        manifest(&tmp, "b", "1", "\"../x/y\"");
        assert!(entrypoint(&tmp.join("a/1")).is_err());
        assert!(entrypoint(&tmp.join("b/1")).is_err());
    }
}
