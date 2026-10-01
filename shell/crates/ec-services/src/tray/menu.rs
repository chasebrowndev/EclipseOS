// SPDX-License-Identifier: AGPL-3.0-only
//! `com.canonical.dbusmenu`, flattened.
//!
//! v1 draws one flat list, so a submenu becomes a header followed by its
//! entries rather than a cascade. Invisible entries are dropped, mnemonics
//! (`_Quit`) are stripped, and runs of separators collapse to one.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

use super::{MenuEntry, MenuKind, Target};
use crate::status::uncached;

const DBUSMENU: &str = "com.canonical.dbusmenu";

/// Bounds on what an app can make us build.
const MAX_ENTRIES: usize = 200;
const MAX_DEPTH: usize = 4;
const MAX_LABEL: usize = 128;

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// Fetch the item's menu. Empty when it has none or never answered.
pub(super) fn read(connection: &Connection, target: &Target) -> Vec<MenuEntry> {
    let Some(path) = target.menu.as_deref() else {
        return Vec::new();
    };
    let Some(menu) = uncached(connection, &target.bus, path, DBUSMENU) else {
        return Vec::new();
    };
    // Some toolkits (Electron, libdbusmenu-qt) fill the menu lazily and only
    // when told it is about to open. Failing that is fine: most do not care.
    let _ = menu.call_method("AboutToShow", &(0i32,));
    let Ok((_revision, (_, _, children))) =
        menu.call::<_, _, (u32, Layout)>("GetLayout", &(0i32, -1i32, Vec::<&str>::new()))
    else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    flatten(&children, 0, &mut entries);
    while entries
        .last()
        .is_some_and(|entry| entry.kind == MenuKind::Separator)
    {
        entries.pop();
    }
    entries
}

/// Tell the app the human picked `entry`.
pub(super) fn click(connection: &Connection, target: &Target, entry: i32) -> zbus::Result<()> {
    let path = target
        .menu
        .as_deref()
        .ok_or_else(|| zbus::Error::Failure("item has no menu".to_owned()))?;
    let menu = uncached(connection, &target.bus, path, DBUSMENU)
        .ok_or_else(|| zbus::Error::Failure("menu unreachable".to_owned()))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u32);
    menu.call_method("Event", &(entry, "clicked", Value::from(0i32), timestamp))
        .map(drop)
}

fn flatten(children: &[OwnedValue], depth: usize, out: &mut Vec<MenuEntry>) {
    for child in children {
        if out.len() >= MAX_ENTRIES {
            return;
        }
        let Some((id, properties, grandchildren)) = child
            .try_clone()
            .ok()
            .and_then(|child| Layout::try_from(child).ok())
        else {
            continue;
        };
        if flag(&properties, "visible") == Some(false) {
            continue;
        }
        if text(&properties, "type").as_deref() == Some("separator") {
            if out.last().is_some_and(|entry| entry.kind != MenuKind::Separator) {
                out.push(MenuEntry {
                    id,
                    label: String::new(),
                    enabled: false,
                    kind: MenuKind::Separator,
                    checked: None,
                });
            }
            continue;
        }
        let label = strip_mnemonic(&text(&properties, "label").unwrap_or_default());
        let submenu =
            text(&properties, "children-display").as_deref() == Some("submenu") || !grandchildren.is_empty();
        if submenu {
            if depth + 1 >= MAX_DEPTH {
                continue;
            }
            out.push(MenuEntry {
                id,
                label,
                enabled: false,
                kind: MenuKind::Header,
                checked: None,
            });
            flatten(&grandchildren, depth + 1, out);
            continue;
        }
        let checked = text(&properties, "toggle-type")
            .filter(|kind| kind == "checkmark" || kind == "radio")
            .map(|_| {
                properties
                    .get("toggle-state")
                    .and_then(|value| value.downcast_ref::<i32>().ok())
                    == Some(1)
            });
        out.push(MenuEntry {
            id,
            label,
            enabled: flag(&properties, "enabled") != Some(false),
            kind: MenuKind::Item,
            checked,
        });
    }
}

fn flag(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    properties.get(name)?.downcast_ref::<bool>().ok()
}

fn text(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties
        .get(name)?
        .downcast_ref::<&str>()
        .ok()
        .map(str::to_owned)
}

/// `_Quit` → `Quit`, `Save __As` → `Save _As`.
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                chars.next();
                out.push('_');
            }
            continue;
        }
        out.push(c);
    }
    out.chars().take(MAX_LABEL).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{Array, Structure, StructureBuilder};

    fn node(
        id: i32,
        properties: &[(&str, Value<'static>)],
        children: Vec<Structure<'static>>,
    ) -> Structure<'static> {
        let properties = properties
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_owned(),
                    OwnedValue::try_from(value.try_clone().unwrap()).unwrap(),
                )
            })
            .collect::<HashMap<String, OwnedValue>>();
        let children = children.into_iter().map(Value::from).collect::<Vec<_>>();
        StructureBuilder::new()
            .add_field(id)
            .add_field(properties)
            .append_field(Value::Array(Array::from(children)))
            .build()
            .unwrap()
    }

    fn owned(children: Vec<Structure<'static>>) -> Vec<OwnedValue> {
        children
            .into_iter()
            .map(|child| OwnedValue::try_from(Value::from(child)).unwrap())
            .collect()
    }

    #[test]
    fn mnemonics_come_out() {
        assert_eq!(strip_mnemonic("_Quit"), "Quit");
        assert_eq!(strip_mnemonic("Save __As"), "Save _As");
    }

    #[test]
    fn a_menu_flattens_to_one_list() {
        let menu = owned(vec![
            node(1, &[("type", Value::from("separator"))], vec![]),
            node(2, &[("label", Value::from("_Open"))], vec![]),
            node(
                3,
                &[("label", Value::from("Hidden")), ("visible", Value::from(false))],
                vec![],
            ),
            node(4, &[("type", Value::from("separator"))], vec![]),
            node(5, &[("type", Value::from("separator"))], vec![]),
            node(
                6,
                &[
                    ("label", Value::from("Mode")),
                    ("children-display", Value::from("submenu")),
                ],
                vec![node(
                    7,
                    &[
                        ("label", Value::from("Dark")),
                        ("toggle-type", Value::from("radio")),
                        ("toggle-state", Value::from(1i32)),
                    ],
                    vec![],
                )],
            ),
            node(
                8,
                &[("label", Value::from("Quit")), ("enabled", Value::from(false))],
                vec![],
            ),
        ]);
        let mut out = Vec::new();
        flatten(&menu, 0, &mut out);
        let shape = out
            .iter()
            .map(|entry| {
                (
                    entry.id,
                    entry.label.as_str(),
                    entry.enabled,
                    entry.kind,
                    entry.checked,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            [
                (2, "Open", true, MenuKind::Item, None),
                (4, "", false, MenuKind::Separator, None),
                (6, "Mode", false, MenuKind::Header, None),
                (7, "Dark", true, MenuKind::Item, Some(true)),
                (8, "Quit", false, MenuKind::Item, None),
            ]
        );
    }
}
