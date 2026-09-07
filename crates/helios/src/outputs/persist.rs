// SPDX-License-Identifier: AGPL-3.0-only
//! Per-output-set layout persistence (COMP-03 §2).
//!
//! The human arranges their monitors once; helios remembers. State lives in
//! `$XDG_STATE_HOME/eclipse/outputs.kdl` (never in the config file — config is
//! hand-written, this is machine-written) and is keyed by the *set* of outputs
//! present, so a laptop docked to two monitors and the same laptop alone get
//! different remembered layouts.
//!
//! Writes are atomic (tmp + rename) and rate-limited to at most one per second.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use kdl::{KdlDocument, KdlValue};

const MIN_WRITE_INTERVAL: Duration = Duration::from_secs(1);

/// What we remember about one output within one set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedOutput {
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    /// width, height, refresh in mHz.
    pub mode: Option<(i32, i32, i32)>,
    pub transform: Option<String>,
    pub enabled: Option<bool>,
}

type Set = BTreeMap<String, SavedOutput>;

#[derive(Debug, Default)]
pub struct Persist {
    path: Option<PathBuf>,
    sets: BTreeMap<String, Set>,
    dirty: bool,
    last_write: Option<Instant>,
}

/// Identify a set of outputs by their sorted identities.
pub fn set_key(identities: &[String]) -> String {
    let mut v: Vec<&str> = identities.iter().map(String::as_str).collect();
    v.sort_unstable();
    v.dedup();
    v.join(" + ")
}

fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("eclipse/outputs.kdl"))
}

impl Persist {
    pub fn load() -> Self {
        let mut p = Self {
            path: state_path(),
            sets: BTreeMap::new(),
            dirty: false,
            last_write: None,
        };
        let Some(path) = p.path.clone() else {
            tracing::warn!("no XDG_STATE_HOME or HOME; output layout will not persist");
            return p;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return p,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "reading output state");
                return p;
            }
        };
        match text.parse::<KdlDocument>() {
            Ok(doc) => p.sets = parse(&doc),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "output state unparseable, ignored"),
        }
        p
    }

    pub fn get(&self, set: &str, identity: &str) -> Option<&SavedOutput> {
        self.sets.get(set).and_then(|s| s.get(identity))
    }

    pub fn record(&mut self, set: &str, identity: &str, saved: SavedOutput) {
        let slot = self.sets.entry(set.to_string()).or_default();
        if slot.get(identity) == Some(&saved) {
            return;
        }
        slot.insert(identity.to_string(), saved);
        self.dirty = true;
    }

    /// Write if anything changed and we have not written in the last second.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(last) = self.last_write {
            if last.elapsed() < MIN_WRITE_INTERVAL {
                return;
            }
        }
        let Some(path) = self.path.clone() else {
            self.dirty = false;
            return;
        };
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(path = %dir.display(), error = %e, "creating state dir");
                return;
            }
        }
        let tmp = path.with_extension("kdl.tmp");
        let body = self.render();
        if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &path)) {
            tracing::warn!(path = %path.display(), error = %e, "writing output state");
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        self.dirty = false;
        self.last_write = Some(Instant::now());
        tracing::debug!(path = %path.display(), "output layout saved");
    }

    fn render(&self) -> String {
        let mut out = String::from("// helios output layout — machine-written, safe to delete\n");
        for (set, outputs) in &self.sets {
            out.push_str(&format!("set {} {{\n", quote(set)));
            for (identity, o) in outputs {
                out.push_str(&format!("    output {} {{\n", quote(identity)));
                if let Some((x, y)) = o.position {
                    out.push_str(&format!("        position {x} {y}\n"));
                }
                if let Some(s) = o.scale {
                    out.push_str(&format!("        scale {s}\n"));
                }
                if let Some((w, h, r)) = o.mode {
                    out.push_str(&format!("        mode \"{w}x{h}@{r}\"\n"));
                }
                if let Some(t) = &o.transform {
                    out.push_str(&format!("        transform {}\n", quote(t)));
                }
                if let Some(e) = o.enabled {
                    out.push_str(&format!("        enabled #{e}\n"));
                }
                out.push_str("    }\n");
            }
            out.push_str("}\n");
        }
        out
    }
}

impl Drop for Persist {
    fn drop(&mut self) {
        self.last_write = None;
        self.flush();
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn parse(doc: &KdlDocument) -> BTreeMap<String, Set> {
    let mut sets = BTreeMap::new();
    for node in doc.nodes() {
        if node.name().value() != "set" {
            continue;
        }
        let Some(key) = first_string(node) else { continue };
        let mut set = Set::new();
        let Some(children) = node.children() else { continue };
        for n in children.nodes() {
            if n.name().value() != "output" {
                continue;
            }
            let Some(identity) = first_string(n) else { continue };
            let mut saved = SavedOutput::default();
            let Some(fields) = n.children() else { continue };
            for f in fields.nodes() {
                let vals: Vec<&KdlValue> = f
                    .entries()
                    .iter()
                    .filter(|e| e.name().is_none())
                    .map(|e| e.value())
                    .collect();
                match f.name().value() {
                    "position" if vals.len() == 2 => {
                        if let (Some(x), Some(y)) = (vals[0].as_integer(), vals[1].as_integer()) {
                            saved.position = Some((x as i32, y as i32));
                        }
                    }
                    "scale" => saved.scale = vals.first().and_then(|v| as_f64(v)),
                    "mode" => saved.mode = vals.first().and_then(|v| v.as_string()).and_then(parse_mode),
                    "transform" => {
                        saved.transform = vals.first().and_then(|v| v.as_string()).map(str::to_string)
                    }
                    "enabled" => saved.enabled = vals.first().and_then(|v| v.as_bool()),
                    other => tracing::debug!(node = other, "unknown output state key, ignored"),
                }
            }
            set.insert(identity, saved);
        }
        sets.insert(key, set);
    }
    sets
}

fn first_string(node: &kdl::KdlNode) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}

fn as_f64(v: &KdlValue) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// `1920x1080` or `1920x1080@60000` (mHz) or `1920x1080@60` (Hz).
pub fn parse_mode(s: &str) -> Option<(i32, i32, i32)> {
    let (dims, refresh) = match s.split_once('@') {
        Some((d, r)) => (d, Some(r)),
        None => (s, None),
    };
    let (w, h) = dims.trim().split_once('x')?;
    let w: i32 = w.trim().parse().ok()?;
    let h: i32 = h.trim().parse().ok()?;
    let r = match refresh {
        None => 0,
        Some(r) => {
            let r = r.trim();
            let hz: f64 = r.parse().ok()?;
            // Anything under 1000 is plainly Hz, not mHz.
            if hz < 1000.0 {
                (hz * 1000.0).round() as i32
            } else {
                hz.round() as i32
            }
        }
    };
    Some((w, h, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(parse_mode("1920x1080"), Some((1920, 1080, 0)));
        assert_eq!(parse_mode("2560x1440@144"), Some((2560, 1440, 144_000)));
        assert_eq!(parse_mode("2560x1440@59951"), Some((2560, 1440, 59_951)));
        assert_eq!(parse_mode("nonsense"), None);
    }

    #[test]
    fn set_keys_are_order_independent() {
        let a = set_key(&["b".into(), "a".into()]);
        let b = set_key(&["a".into(), "b".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn round_trips() {
        let mut p = Persist::default();
        p.sets.insert(
            "A + B".into(),
            [(
                "A".to_string(),
                SavedOutput {
                    position: Some((1920, 0)),
                    scale: Some(1.5),
                    mode: Some((2560, 1440, 144_000)),
                    transform: Some("90".into()),
                    enabled: Some(true),
                },
            )]
            .into_iter()
            .collect(),
        );
        let text = p.render();
        let doc: KdlDocument = text.parse().expect("renders valid kdl");
        let back = parse(&doc);
        assert_eq!(back, p.sets);
    }
}
