// SPDX-License-Identifier: AGPL-3.0-only
//! The component catalog (D-07 §4.1): the only source of package and unit names
//! that come from a user's choice. The caller sends ids; this resolves them.
//!
//! M1 ships a built-in minimal set. The read-only root-owned
//! `/usr/share/eclipse/setup/catalog.kdl` replaces it in M2; the resolution
//! contract (`resolve` takes ids, returns entries, refuses everything else) does
//! not change.

use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Bar,
    Launcher,
    Notifications,
    ControlCenter,
    /// Optional applications: any number may be chosen, none exclude another.
    App,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: &'static str,
    pub slot: Slot,
    /// Packages this candidate needs on top of the floor.
    pub packages: &'static [&'static str],
    /// **System** units to enable. The M1 set has none: the DE components are
    /// user units that their packages already `want` from `abyss-session.target`.
    pub system_units: &'static [&'static str],
}

pub struct Catalog {
    entries: &'static [Entry],
}

const BUILTIN: &[Entry] = &[
    Entry {
        id: "ec-hyperion-bar",
        slot: Slot::Bar,
        packages: &["eclipseos-hyperion"],
        system_units: &[],
    },
    Entry {
        id: "ec-launcher",
        slot: Slot::Launcher,
        packages: &["eclipseos-launcher"],
        system_units: &[],
    },
    Entry {
        id: "ec-toasts",
        slot: Slot::Notifications,
        packages: &["eclipseos-toasts"],
        system_units: &[],
    },
    Entry {
        id: "ec-center",
        slot: Slot::ControlCenter,
        packages: &["eclipseos-center"],
        system_units: &[],
    },
    Entry {
        id: "waybar",
        slot: Slot::Bar,
        packages: &["waybar"],
        system_units: &[],
    },
    Entry {
        id: "quickshell",
        slot: Slot::Bar,
        packages: &["quickshell"],
        system_units: &[],
    },
    Entry {
        id: "fuzzel",
        slot: Slot::Launcher,
        packages: &["fuzzel"],
        system_units: &[],
    },
    Entry {
        id: "mako",
        slot: Slot::Notifications,
        packages: &["mako"],
        system_units: &[],
    },
    Entry {
        id: "app-browser",
        slot: Slot::App,
        packages: &["firefox"],
        system_units: &[],
    },
    Entry {
        id: "app-editor",
        slot: Slot::App,
        packages: &["micro"],
        system_units: &[],
    },
    Entry {
        id: "app-files",
        slot: Slot::App,
        packages: &["thunar"],
        system_units: &[],
    },
    Entry {
        id: "app-media",
        slot: Slot::App,
        packages: &["mpv"],
        system_units: &[],
    },
    Entry {
        id: "app-images",
        slot: Slot::App,
        packages: &["imv"],
        system_units: &[],
    },
    Entry {
        id: "app-pdf",
        slot: Slot::App,
        packages: &["zathura", "zathura-pdf-mupdf"],
        system_units: &[],
    },
    Entry {
        id: "app-archive",
        slot: Slot::App,
        packages: &["p7zip", "unzip"],
        system_units: &[],
    },
    Entry {
        id: "app-printing",
        slot: Slot::App,
        packages: &["cups"],
        system_units: &["cups.socket"],
    },
    Entry {
        id: "app-bluetooth-ui",
        slot: Slot::App,
        packages: &["blueman"],
        system_units: &[],
    },
];

/// System units of the floor (the session and network the installed system
/// cannot boot usefully without), enabled on every install regardless of the
/// candidates. Fixed here, like the catalog: nothing the caller says adds to it.
/// `iwd` is not among them: the live medium's wifi is not carried over (D-07
/// §4.3), and without that NetworkManager backend switch iwd only races it.
pub const FLOOR_UNITS: &[&str] = &[
    "greetd",
    "NetworkManager",
    "bluetooth",
    "systemd-timesyncd",
    "fstrim.timer",
];

/// The agent stack add-on (ADR 0069), installed only when the plan's `agents`
/// flag is set (D-07 §4.4 "Stack"). A constant like the catalog: the flag is a
/// yes/no, never a name.
pub const AGENTS_PACKAGE: &str = "eclipseos-agents";

/// More than there are slots is already nonsense.
const MAX_CANDIDATES: usize = 32;

impl Catalog {
    pub fn builtin() -> Self {
        Catalog { entries: BUILTIN }
    }

    pub fn entries(&self) -> &'static [Entry] {
        self.entries
    }

    /// Ids in, entries out. Exact match only; an unknown id, a duplicate or two
    /// candidates for one slot is a refusal, never a best guess.
    pub fn resolve(&self, ids: &[String]) -> Result<Vec<&'static Entry>> {
        if ids.len() > MAX_CANDIDATES {
            return Err(Error::Refused("too many candidates"));
        }
        let mut out: Vec<&'static Entry> = Vec::with_capacity(ids.len());
        for id in ids {
            let e = self
                .entries
                .iter()
                .find(|e| e.id == id.as_str())
                .ok_or(Error::Refused("candidate not in catalog"))?;
            if out
                .iter()
                .any(|o| o.id == e.id || (o.slot == e.slot && e.slot != Slot::App))
            {
                return Err(Error::Refused("two candidates for one slot"));
            }
            out.push(e);
        }
        Ok(out)
    }
}

/// Package and unit names in the catalog are held to the same pattern as the
/// medium's package list: they end up as argv elements.
pub fn valid_pkg_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with(['-', '.'])
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'@' | b'.' | b'_' | b'+' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn builtin_names_are_argv_safe() {
        for e in Catalog::builtin().entries() {
            assert!(valid_pkg_name(e.id));
            for p in e.packages.iter().chain(e.system_units) {
                assert!(valid_pkg_name(p), "{p}");
            }
        }
        assert!(valid_pkg_name(AGENTS_PACKAGE));
        for u in FLOOR_UNITS {
            assert!(valid_pkg_name(u), "{u}");
        }
    }

    #[test]
    fn resolves_known_ids() {
        let c = Catalog::builtin();
        let r = c.resolve(&ids(&["ec-hyperion-bar", "ec-toasts"])).unwrap();
        assert_eq!(r.len(), 2);
        assert!(c.resolve(&[]).unwrap().is_empty());
    }

    #[test]
    fn refuses_unknown_duplicate_and_package_names() {
        let c = Catalog::builtin();
        // A raw package name is not an id, even a real one.
        for bad in [
            "firefox",
            "eclipseos-hyperion",
            "hyperion ",
            "Hyperion",
            "",
            "hyperion;reboot",
        ] {
            assert!(c.resolve(&ids(&[bad])).is_err(), "{bad:?}");
        }
        assert!(c.resolve(&ids(&["ec-hyperion-bar", "ec-hyperion-bar"])).is_err());
        assert!(c.resolve(&ids(&["a"; 33])).is_err());
    }

    #[test]
    fn apps_stack_but_slots_do_not() {
        let c = Catalog::builtin();
        assert_eq!(
            c.resolve(&ids(&["app-browser", "app-pdf", "mako"]))
                .unwrap()
                .len(),
            3
        );
        assert!(c.resolve(&ids(&["ec-hyperion-bar", "waybar"])).is_err());
        assert!(c.resolve(&ids(&["app-pdf", "app-pdf"])).is_err());
    }

    #[test]
    fn pkg_names() {
        assert!(valid_pkg_name("gtk3"));
        assert!(valid_pkg_name("libc++"));
        for bad in ["", "-rf", ".x", "a b", "a;b", "a/b", "a\n", "a$b"] {
            assert!(!valid_pkg_name(bad), "{bad:?}");
        }
    }
}
