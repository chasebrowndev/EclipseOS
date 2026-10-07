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

use ec_inference_wire::Backend;

use crate::sandbox::RawDecl;

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
    find(roots, id, version).map(|(d, _)| d)
}

/// The same, with the publisher of the root that holds it.
pub fn find(roots: &[(PathBuf, String)], id: &str, version: &str) -> Option<(PathBuf, String)> {
    if !safe_component(id) || !safe_component(version) {
        return None;
    }
    roots
        .iter()
        .map(|(r, p)| (r.join(id).join(version), p.clone()))
        .find(|(d, _)| d.join("manifest.kdl").is_file())
}

/// Whether a closed task of this package may be resumed (A-08 §5.4, F-24):
/// the manifest's `resumable`, which defaults to true for publisher `local`
/// and false for everything else.
pub fn resumable(pkg_dir: &Path, publisher: &str) -> bool {
    read_manifest(pkg_dir)
        .and_then(|m| m.get_arg("resumable").and_then(|v| v.as_bool()))
        .unwrap_or(publisher == "local")
}

/// The manifest's `inference { backend; model }` (A-07, ADR 0076): which
/// backend and model a package's tasks may ask the inference router for.
/// agentd stamps these on every request; the agent never names them.
#[derive(Debug, Clone, PartialEq)]
pub struct Inference {
    pub backend: Backend,
    pub model: String,
}

/// Reads the `inference` block with policyd's rules (`ec-policyd`'s manifest
/// parser): both keys required and single-valued, backend `api` or
/// `claude-code`, model ASCII alphanumerics, `-` and `.` up to 64 bytes,
/// nothing else in the block. `Ok(None)` when the manifest declares none; an
/// invalid block is an error, so the package gets no inference tool.
pub fn inference(pkg_dir: &Path) -> Result<Option<Inference>, String> {
    let m = read_manifest(pkg_dir).ok_or("no readable manifest")?;
    let mut nodes = m.nodes().iter().filter(|n| n.name().value() == "inference");
    let Some(block) = nodes.next() else {
        return Ok(None);
    };
    if nodes.next().is_some() {
        return Err("inference declared twice".into());
    }
    let (mut backend, mut model) = (None::<String>, None::<String>);
    for c in block.children().map(|d| d.nodes()).unwrap_or_default() {
        let slot = match c.name().value() {
            "backend" => &mut backend,
            "model" => &mut model,
            other => return Err(format!("unknown node inference.{other}")),
        };
        let mut args = c.entries().iter().filter(|e| e.name().is_none());
        let (Some(a), None) = (args.next(), args.next()) else {
            return Err("inference keys take one value".into());
        };
        if slot.is_some() || c.entries().iter().any(|e| e.name().is_some()) {
            return Err("inference key repeated or has properties".into());
        }
        *slot = Some(
            a.value()
                .as_string()
                .ok_or("inference values are strings")?
                .to_owned(),
        );
    }
    let backend = backend.ok_or("inference.backend is required")?;
    let model = model.ok_or("inference.model is required")?;
    let backend = Backend::parse(&backend).ok_or("inference.backend must be api or claude-code")?;
    if model.is_empty()
        || model.len() > 64
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return Err("inference.model is malformed".into());
    }
    Ok(Some(Inference { backend, model }))
}

/// The manifest's `sandbox { fs.read ...; fs.write ...; net.egress ... }`
/// (A-07 §2). An unknown key is an error: fail closed rather than launch a
/// package whose sandbox request this build cannot read.
pub fn sandbox_decl(pkg_dir: &Path) -> Result<RawDecl, String> {
    let m = read_manifest(pkg_dir).ok_or("no readable manifest")?;
    let mut d = RawDecl::default();
    let Some(block) = m.get("sandbox") else {
        return Ok(d);
    };
    for n in block.children().map(|c| c.nodes()).unwrap_or_default() {
        let word = n
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .and_then(|e| e.value().as_string())
            .ok_or("sandbox_invalid")?
            .to_owned();
        match n.name().value() {
            "fs.read" => d.fs_read.push(word),
            "fs.write" => d.fs_write.push(word),
            "net.egress" => d.egress.push(word),
            _ => return Err("sandbox_unknown_key".into()),
        }
    }
    Ok(d)
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
    fn reads_the_sandbox_block_and_resumable() {
        let tmp = crate::scratch_dir("pkgs-sbx");
        let d = tmp.join("a/1");
        std::fs::create_dir_all(&d).unwrap();
        let write = |body: &str| {
            std::fs::write(
                d.join("manifest.kdl"),
                format!("agent {{\n  id \"a\"\n  version \"1\"\n  entrypoint \"x\"\n{body}\n}}\n"),
            )
            .unwrap()
        };
        write("");
        assert_eq!(sandbox_decl(&d).unwrap(), RawDecl::default());
        assert!(resumable(&d, "local") && !resumable(&d, "eclipse"));
        write(
            "  resumable #false\n  sandbox {\n    fs.read \"~/Documents/invoices\"\n    fs.write \"/tmp/o\"\n    net.egress \"api.acme.com:443\"\n  }",
        );
        let s = sandbox_decl(&d).unwrap();
        assert_eq!(s.fs_read, ["~/Documents/invoices"]);
        assert_eq!(s.fs_write, ["/tmp/o"]);
        assert_eq!(s.egress, ["api.acme.com:443"]);
        assert!(!resumable(&d, "local"));
        write("  resumable #true");
        assert!(resumable(&d, "eclipse"));
        write("  sandbox {\n    fs.exec \"/x\"\n  }");
        assert_eq!(sandbox_decl(&d).unwrap_err(), "sandbox_unknown_key");
    }

    #[test]
    fn reads_the_inference_block_with_policyds_rules() {
        let tmp = crate::scratch_dir("pkgs-inf");
        let d = tmp.join("a/1");
        std::fs::create_dir_all(&d).unwrap();
        let write = |body: &str| {
            std::fs::write(
                d.join("manifest.kdl"),
                format!("agent {{\n  id \"a\"\n  version \"1\"\n  entrypoint \"x\"\n{body}\n}}\n"),
            )
            .unwrap()
        };
        let block = |inner: &str| format!("  inference {{\n{inner}\n  }}");
        write("");
        assert_eq!(inference(&d), Ok(None));
        write(&block("    backend \"api\"\n    model \"claude-opus-4.5\""));
        assert_eq!(
            inference(&d),
            Ok(Some(Inference {
                backend: Backend::Api,
                model: "claude-opus-4.5".into()
            }))
        );
        let long = format!("    backend \"api\"\n    model \"{}\"", "m".repeat(65));
        for bad in [
            block("    backend \"api\""),
            block("    model \"m\""),
            block("    backend \"gpt\"\n    model \"m\""),
            block("    backend \"api\"\n    model \"a/b\""),
            block("    backend \"api\"\n    model \"\""),
            block("    backend \"api\"\n    backend \"api\"\n    model \"m\""),
            block("    backend \"api\"\n    model \"m\"\n    url \"x\""),
            block(&long),
            format!(
                "{}\n{}",
                block("    backend \"api\"\n    model \"m\""),
                block("    backend \"api\"\n    model \"m\"")
            ),
        ] {
            write(&bad);
            assert!(inference(&d).is_err(), "{bad}");
        }
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
