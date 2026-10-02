// SPDX-License-Identifier: AGPL-3.0-only
//! Reading one `StatusNotifierItem` into a [`TrayItem`].
//!
//! Everything here is app-supplied, so everything is bounded: a pixmap that
//! does not add up is dropped, and one that is huge is never copied.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedValue};

use super::{ItemStatus, Pixmap, Target, TrayItem};
use crate::status::uncached;

/// KDE's name is the one every toolkit uses; the freedesktop spelling shows
/// up from a few older libraries.
const INTERFACES: [&str; 2] = ["org.kde.StatusNotifierItem", "org.freedesktop.StatusNotifierItem"];

/// No bar icon needs more than this, and a 256² ARGB image is already 256 KiB
/// per item per read.
const MAX_PIXMAP_SIDE: i32 = 256;
/// The size a bar draws at, give or take hidpi. The pixmap closest above it is
/// the sharpest one that is not wastefully large.
const WANTED_SIDE: i32 = 32;
const MAX_TEXT: usize = 256;

/// Read the item at `entry` (`bus/path`). `None` if it is gone or never
/// answered — the bar skips it rather than drawing a blank.
pub(super) fn read(connection: &Connection, entry: &str) -> Option<(TrayItem, Target)> {
    let (bus, path) = super::watcher::split_entry(entry);
    let properties = uncached(connection, bus, path, "org.freedesktop.DBus.Properties")?;
    let (interface, all) = INTERFACES.iter().find_map(|interface| {
        properties
            .call::<_, _, HashMap<String, OwnedValue>>("GetAll", &(*interface,))
            .ok()
            .map(|all| (*interface, all))
    })?;

    let status = match text(&all, "Status").as_str() {
        "Passive" => ItemStatus::Passive,
        "NeedsAttention" => ItemStatus::NeedsAttention,
        _ => ItemStatus::Active,
    };
    let attention = status == ItemStatus::NeedsAttention;
    let icon_name = Some(text(&all, "AttentionIconName"))
        .filter(|name| attention && !name.is_empty())
        .unwrap_or_else(|| text(&all, "IconName"));
    let icon_pixmap = attention
        .then(|| pixmap(&all, "AttentionIconPixmap"))
        .flatten()
        .or_else(|| pixmap(&all, "IconPixmap"));
    let menu = all
        .get("Menu")
        .and_then(|value| value.downcast_ref::<ObjectPath<'_>>().ok())
        .map(|path| path.to_string())
        .filter(|path| path != "/");
    let title = Some(text(&all, "Title"))
        .filter(|title| !title.is_empty())
        .or_else(|| tooltip_title(&all))
        .unwrap_or_default();
    let id = Some(text(&all, "Id"))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| entry.to_owned());

    let item = TrayItem {
        title: if title.is_empty() { id.clone() } else { title },
        id,
        address: entry.to_owned(),
        status,
        icon_name,
        icon_theme_path: text(&all, "IconThemePath"),
        icon_pixmap,
        item_is_menu: all
            .get("ItemIsMenu")
            .and_then(|value| value.downcast_ref::<bool>().ok())
            .unwrap_or(false),
        has_menu: menu.is_some(),
    };
    let target = Target {
        bus: bus.to_owned(),
        path: path.to_owned(),
        interface,
        menu,
    };
    Some((item, target))
}

fn text(all: &HashMap<String, OwnedValue>, name: &str) -> String {
    all.get(name)
        .and_then(|value| value.downcast_ref::<&str>().ok())
        .map(|text| text.chars().take(MAX_TEXT).collect())
        .unwrap_or_default()
}

/// `ToolTip` is `(icon name, icon pixmaps, title, body)`; some apps set only
/// that and leave `Title` empty.
fn tooltip_title(all: &HashMap<String, OwnedValue>) -> Option<String> {
    type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);
    let tooltip = ToolTip::try_from(all.get("ToolTip")?.try_clone().ok()?).ok()?;
    Some(tooltip.2.chars().take(MAX_TEXT).collect()).filter(|title: &String| !title.is_empty())
}

fn pixmap(all: &HashMap<String, OwnedValue>, name: &str) -> Option<Pixmap> {
    let candidates = Vec::<(i32, i32, Vec<u8>)>::try_from(all.get(name)?.try_clone().ok()?).ok()?;
    pick(candidates)
}

/// The smallest pixmap at least [`WANTED_SIDE`] across, else the largest
/// there is — converted from the spec's big-endian ARGB to the RGBA a GUI
/// image wants.
fn pick(candidates: Vec<(i32, i32, Vec<u8>)>) -> Option<Pixmap> {
    let valid = candidates.into_iter().filter(|(width, height, bytes)| {
        (1..=MAX_PIXMAP_SIDE).contains(width)
            && (1..=MAX_PIXMAP_SIDE).contains(height)
            && usize::try_from(width * height * 4).is_ok_and(|len| len == bytes.len())
    });
    let (width, height, argb) = valid.min_by_key(|(width, height, _)| {
        let side = (*width).max(*height);
        // Big-enough ones first, smallest of those; then the rest, largest first.
        if side >= WANTED_SIDE {
            (0, side)
        } else {
            (1, -side)
        }
    })?;
    let rgba = argb
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[1], pixel[2], pixel[3], pixel[0]])
        .collect();
    Some(Pixmap {
        width: width.unsigned_abs(),
        height: height.unsigned_abs(),
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pixmap_is_the_smallest_sharp_one_in_rgba() {
        let solid = |side: i32| {
            (
                side,
                side,
                [0xFF, 0x10, 0x20, 0x30].repeat((side * side) as usize),
            )
        };
        let picked = pick(vec![solid(16), solid(64), solid(48), solid(22)]).unwrap();
        assert_eq!((picked.width, picked.height), (48, 48));
        assert_eq!(&picked.rgba[..4], &[0x10, 0x20, 0x30, 0xFF]);

        // Nothing big enough: the largest small one.
        let picked = pick(vec![solid(16), solid(22)]).unwrap();
        assert_eq!(picked.width, 22);
    }

    #[test]
    fn a_pixmap_that_does_not_add_up_is_dropped() {
        assert!(pick(vec![(4, 4, vec![0; 10])]).is_none());
        assert!(pick(vec![(0, 4, Vec::new())]).is_none());
        assert!(pick(vec![(-2, -2, vec![0; 16])]).is_none());
        assert!(pick(vec![(512, 1, vec![0; 2048])]).is_none());
    }
}
