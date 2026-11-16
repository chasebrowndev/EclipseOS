// SPDX-License-Identifier: AGPL-3.0-only
//! The schema table (T-10 §1). A key that is not in [`TABLE`] does not exist.

use std::collections::HashMap;
use std::sync::OnceLock;

pub use crate::rows::TABLE;
pub use crate::types::{Dv, Key, OverrideClass, Reload, Scope, Ty, Value};

/// Lists whose user value extends the built-in default instead of replacing it.
pub const EXTEND_ONLY: &[&str] = &["remote.clients", "socket.interpreters"];

fn index() -> &'static HashMap<&'static str, usize> {
    static IDX: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    IDX.get_or_init(|| TABLE.iter().enumerate().map(|(i, k)| (k.path, i)).collect())
}

/// Position of a key in [`TABLE`].
pub fn index_of(path: &str) -> Option<usize> {
    index().get(path).copied()
}

/// The row of a key.
pub fn key(path: &str) -> Option<&'static Key> {
    index_of(path).map(|i| &TABLE[i])
}

/// Is `path` an interior node of the key tree (some key lives below it)?
pub fn is_container(path: &str) -> bool {
    let prefix = format!("{path}.");
    TABLE.iter().any(|k| k.path.starts_with(&prefix))
}

/// The immediate child names below `prefix` (`""` is the top level), sorted, deduplicated.
pub fn children_of(prefix: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for k in TABLE {
        let rest = if prefix.is_empty() {
            k.path
        } else {
            match k
                .path
                .strip_prefix(prefix)
                .and_then(|r| r.strip_prefix('.'))
            {
                Some(r) => r,
                None => continue,
            }
        };
        let seg = rest.split('.').next().unwrap_or(rest);
        if !out.contains(&seg) {
            out.push(seg);
        }
    }
    out.sort_unstable();
    out
}

/// The override class of a path, fail-closed: an unknown path is `Grant`.
pub fn class_of(path: &str) -> OverrideClass {
    key(path).map(|k| k.class).unwrap_or(OverrideClass::Grant)
}

/// Fraction of keys with reload class `Live`.
pub fn live_fraction() -> f64 {
    let live = TABLE.iter().filter(|k| k.reload == Reload::Live).count();
    live as f64 / TABLE.len() as f64
}
