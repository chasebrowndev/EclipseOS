// SPDX-License-Identifier: AGPL-3.0-only
//! Config diffs (T-10 §4): which keys changed and the strictest reload class among them.

use crate::model::{Config, Spanned};
use crate::schema::TABLE;
use crate::types::{Reload, Value};

/// A collection that changed (binds, rules, ...). All collections apply live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collection {
    Binds,
    Unbinds,
    MouseBinds,
    KeyTables,
    Macros,
    Rules,
    Profiles,
    Layouts,
    Menus,
    Domains,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyChange {
    pub path: &'static str,
    pub old: Value,
    pub new: Value,
    pub reload: Reload,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigDiff {
    pub keys: Vec<KeyChange>,
    pub collections: Vec<Collection>,
}

fn same<T: PartialEq>(a: &[Spanned<T>], b: &[Spanned<T>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.value == y.value)
}

fn rank(r: Reload) -> u8 {
    match r {
        Reload::Live => 0,
        Reload::NewPane => 1,
        Reload::NewWindow => 2,
        Reload::Restart => 3,
    }
}

impl ConfigDiff {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.collections.is_empty()
    }

    /// The strictest reload class among the changes; `Live` for an empty diff.
    pub fn reload_class(&self) -> Reload {
        self.keys
            .iter()
            .map(|k| k.reload)
            .max_by_key(|r| rank(*r))
            .unwrap_or(Reload::Live)
    }

    /// Changes that take effect on the running terminal now.
    pub fn live(&self) -> impl Iterator<Item = &KeyChange> {
        self.keys.iter().filter(|k| k.reload == Reload::Live)
    }

    /// Changes that wait for a new pane, window or restart.
    pub fn deferred(&self) -> impl Iterator<Item = &KeyChange> {
        self.keys.iter().filter(|k| k.reload != Reload::Live)
    }
}

/// Compare two configs. A key whose value is unchanged is not listed, whatever its origin.
pub fn diff(old: &Config, new: &Config) -> ConfigDiff {
    let mut d = ConfigDiff::default();
    for (i, k) in TABLE.iter().enumerate() {
        let (a, b) = (&old.slots[i].value, &new.slots[i].value);
        if a != b {
            d.keys.push(KeyChange {
                path: k.path,
                old: a.clone(),
                new: b.clone(),
                reload: k.reload,
            });
        }
    }
    let checks = [
        (Collection::Binds, same(&old.binds, &new.binds)),
        (Collection::Unbinds, same(&old.unbinds, &new.unbinds)),
        (
            Collection::MouseBinds,
            same(&old.mouse_binds, &new.mouse_binds),
        ),
        (
            Collection::KeyTables,
            same(&old.key_tables, &new.key_tables),
        ),
        (Collection::Macros, same(&old.macros, &new.macros)),
        (Collection::Rules, same(&old.rules, &new.rules)),
        (Collection::Profiles, same(&old.profiles, &new.profiles)),
        (Collection::Layouts, same(&old.layouts, &new.layouts)),
        (Collection::Menus, same(&old.menus, &new.menus)),
        (Collection::Domains, same(&old.domains, &new.domains)),
    ];
    for (c, eq) in checks {
        if !eq {
            d.collections.push(c);
        }
    }
    d
}

/// Keys that differ from the built-in defaults (`apoth config diff`, `config show --changed`).
pub fn diff_from_defaults(cfg: &Config) -> ConfigDiff {
    diff(&Config::defaults(), cfg)
}
