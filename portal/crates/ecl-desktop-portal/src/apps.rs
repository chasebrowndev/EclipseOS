// SPDX-License-Identifier: AGPL-3.0-only

//! Reading the XDG application database: `.desktop` files, `mimeapps.list`,
//! `mimeinfo.cache` and shared-mime-info globs. Text goes to the pure parsers
//! in `ecl-portal-core`.

use std::fs;
use std::path::PathBuf;

use ecl_portal_core::identity::{parse_desktop, DesktopEntry, DesktopIndex};
use ecl_portal_core::openuri::{mime_from_globs2, MimeApps};

fn env_abs(k: &str) -> Option<PathBuf> {
    std::env::var(k)
        .ok()
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
}

fn list_env(k: &str, default: &str) -> Vec<PathBuf> {
    std::env::var(k)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
        .split(':')
        .filter(|p| p.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

pub fn home() -> Option<PathBuf> {
    env_abs("HOME")
}

pub fn data_home() -> Option<PathBuf> {
    env_abs("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local/share")))
}

pub fn config_home() -> Option<PathBuf> {
    env_abs("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config")))
}

/// Data dirs, highest priority first.
pub fn data_dirs() -> Vec<PathBuf> {
    data_home()
        .into_iter()
        .chain(list_env("XDG_DATA_DIRS", "/usr/local/share:/usr/share"))
        .collect()
}

fn config_dirs() -> Vec<PathBuf> {
    config_home()
        .into_iter()
        .chain(list_env("XDG_CONFIG_DIRS", "/etc/xdg"))
        .collect()
}

/// Index of every `applications/*.desktop` (top level only).
pub fn load_index() -> DesktopIndex {
    let mut idx = DesktopIndex::new();
    // Lowest priority first so higher-priority dirs overwrite.
    for d in data_dirs().into_iter().rev() {
        let Ok(rd) = fs::read_dir(d.join("applications")) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".desktop") {
                continue;
            }
            if let Ok(t) = fs::read_to_string(e.path()) {
                idx.insert(&name, parse_desktop(&t));
            }
        }
    }
    idx
}

/// Parsed entry for a desktop id, from the highest-priority data dir.
pub fn desktop_entry(id: &str) -> Option<DesktopEntry> {
    if id.contains('/') {
        return None;
    }
    data_dirs().into_iter().find_map(|d| {
        fs::read_to_string(d.join("applications").join(id))
            .ok()
            .map(|t| parse_desktop(&t))
    })
}

/// `mimeapps.list` and `mimeinfo.cache` merged in spec priority order.
pub fn mime_apps() -> MimeApps {
    let mut m = MimeApps::default();
    let mut lists: Vec<PathBuf> = Vec::new();
    for c in config_dirs() {
        lists.push(c.join("mimeapps.list"));
    }
    for d in data_dirs() {
        lists.push(d.join("applications/mimeapps.list"));
    }
    for p in lists {
        if let Ok(t) = fs::read_to_string(p) {
            m.merge_lower(&MimeApps::parse(&t));
        }
    }
    for d in data_dirs() {
        if let Ok(t) = fs::read_to_string(d.join("applications/mimeinfo.cache")) {
            m.merge_lower(&MimeApps::parse_cache(&t));
        }
    }
    m
}

/// MIME type for a file name from the shared-mime-info globs.
pub fn mime_of_name(name: &str) -> Option<String> {
    data_dirs().into_iter().find_map(|d| {
        fs::read_to_string(d.join("mime/globs2"))
            .ok()
            .and_then(|t| mime_from_globs2(&t, name))
    })
}
