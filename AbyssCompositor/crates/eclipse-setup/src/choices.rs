// SPDX-License-Identifier: AGPL-3.0-only
//! What a profile pre-selects, and what the user changes from there (COMP-17
//! §2.1, ADR 0060). A profile is a one-time seed: [`Choices::for_profile`] is
//! the only place a profile is read, every value it picks is then an ordinary
//! field the steps edit, and nothing looks at the profile again.
//!
//! Everything that leaves this module is either a config key and value (the
//! seed the helper copies, D-07 §5) or a catalog id (the helper resolves it,
//! D-07 §4.1, §6). No package name, unit name or command exists here.

use eclipse_setup_plan::Profile;
use serde_json::{json, Value};

/// `mode` (ADR 0062): what the session looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Wm,
    Hybrid,
    De,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Wm, Mode::Hybrid, Mode::De];

    pub fn id(self) -> &'static str {
        match self {
            Mode::Wm => "wm",
            Mode::Hybrid => "hybrid",
            Mode::De => "de",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Wm => "Windows only",
            Mode::Hybrid => "Windows and a taskbar",
            Mode::De => "Full desktop",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Mode::Wm => "A bar that shows workspaces and nothing else.",
            Mode::Hybrid => "The bar also lists your open windows.",
            Mode::De => "Everything in the taskbar mode, plus icons on the desktop.",
        }
    }
}

/// `general.layout`: how new windows are placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiling {
    Radiant,
    Dwindle,
    Master,
}

impl Tiling {
    pub const ALL: [Tiling; 3] = [Tiling::Radiant, Tiling::Dwindle, Tiling::Master];

    pub fn id(self) -> &'static str {
        match self {
            Tiling::Radiant => "radiant",
            Tiling::Dwindle => "dwindle",
            Tiling::Master => "master",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Tiling::Radiant => "Drag a window to where it should go.",
            Tiling::Dwindle => "Each new window splits the last one in half.",
            Tiling::Master => "One main window on the left, the rest stacked.",
        }
    }
}

/// `bar.position`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
}

impl BarPosition {
    pub fn id(self) -> &'static str {
        match self {
            BarPosition::Top => "top",
            BarPosition::Bottom => "bottom",
        }
    }
}

/// A replaceable piece of the desktop (D-07 §4.1). The last choice is always
/// [`NONE`], and it is the only entry that is not a catalog id.
pub struct Slot {
    /// The `components.` key.
    pub key: &'static str,
    pub label: &'static str,
    pub choices: &'static [&'static str],
}

/// A slot with nothing in it. Never sent to the helper.
pub const NONE: &str = "none";

pub const SLOTS: [Slot; 4] = [
    Slot {
        key: "components.bar",
        label: "Bar",
        choices: &["hyperion", "waybar", "quickshell", NONE],
    },
    Slot {
        key: "components.launcher",
        label: "Launcher",
        choices: &["eclipse-launcher", "fuzzel", NONE],
    },
    Slot {
        key: "components.notifications",
        label: "Notifications",
        choices: &["eclipse-toasts", "mako", NONE],
    },
    Slot {
        key: "components.control-center",
        label: "Control center",
        choices: &["eclipse-center", NONE],
    },
];

/// The bar slot's index in [`SLOTS`].
pub const BAR: usize = 0;

/// An optional application, as a catalog id and the plain name the screen shows.
pub struct App {
    pub id: &'static str,
    pub name: &'static str,
}

pub const APPS: [App; 9] = [
    App {
        id: "app-browser",
        name: "Web browser",
    },
    App {
        id: "app-editor",
        name: "Text editor",
    },
    App {
        id: "app-files",
        name: "File manager",
    },
    App {
        id: "app-media",
        name: "Music and video",
    },
    App {
        id: "app-images",
        name: "Image viewer",
    },
    App {
        id: "app-pdf",
        name: "PDF reader",
    },
    App {
        id: "app-archive",
        name: "Zip and tar files",
    },
    App {
        id: "app-printing",
        name: "Printing",
    },
    App {
        id: "app-bluetooth-ui",
        name: "Bluetooth manager",
    },
];

/// Standard and Agentic ask and pre-tick nothing; Full ticks a working set.
const FULL_APPS: [&str; 7] = [
    "app-browser",
    "app-editor",
    "app-files",
    "app-media",
    "app-images",
    "app-pdf",
    "app-archive",
];

/// `decoration.rounding` when corners are rounded (the shipped default).
pub const ROUNDING_ON: i64 = 13;

/// Every choice the steps after Profile edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices {
    pub mode: Mode,
    pub tiling: Tiling,
    /// One entry per [`SLOTS`] row: a member of that slot's `choices`.
    pub slots: [&'static str; 4],
    pub rounded: bool,
    pub blur: bool,
    pub animations: bool,
    pub bar_position: BarPosition,
    /// One flag per [`APPS`] row.
    pub apps: [bool; 9],
}

impl Choices {
    /// The profile's defaults (COMP-17 §2.1 table).
    pub fn for_profile(p: Profile) -> Choices {
        let minimal = p == Profile::Minimal;
        let native = |id: &'static str| if minimal { NONE } else { id };
        let mut apps = [false; 9];
        if p == Profile::Full {
            for (on, app) in apps.iter_mut().zip(&APPS) {
                *on = FULL_APPS.contains(&app.id);
            }
        }
        Choices {
            mode: if minimal { Mode::Wm } else { Mode::Hybrid },
            tiling: Tiling::Radiant,
            slots: [
                native("hyperion"),
                // A session with no launcher cannot start anything.
                "eclipse-launcher",
                native("eclipse-toasts"),
                native("eclipse-center"),
            ],
            rounded: true,
            blur: true,
            animations: false,
            bar_position: BarPosition::Top,
            apps,
        }
    }

    /// Catalog ids for the plan: the chosen components, then the ticked
    /// applications. `none` is not an id.
    pub fn candidates(&self) -> Vec<String> {
        let comps = self.slots.iter().filter(|s| **s != NONE).map(|s| (*s).to_owned());
        let apps = APPS
            .iter()
            .zip(&self.apps)
            .filter(|(_, on)| **on)
            .map(|(a, _)| a.id.to_owned());
        comps.chain(apps).collect()
    }

    /// Every seed key with its value, in a fixed order.
    pub fn seeds(&self) -> Vec<(&'static str, Value)> {
        let mut v = vec![(MODE, self.mode_value()), (LAYOUT, self.tiling_value())];
        for (i, slot) in SLOTS.iter().enumerate() {
            v.push((slot.key, self.slot_value(i)));
        }
        v.push((ROUNDING, self.rounding_value()));
        v.push((BLUR, self.blur_value()));
        v.push((ANIMATIONS, self.animations_value()));
        v.push((BAR_POSITION, self.bar_position_value()));
        v
    }

    pub fn mode_value(&self) -> Value {
        json!(self.mode.id())
    }
    pub fn tiling_value(&self) -> Value {
        json!(self.tiling.id())
    }
    pub fn slot_value(&self, slot: usize) -> Value {
        json!(self.slots[slot])
    }
    pub fn rounding_value(&self) -> Value {
        json!(if self.rounded { ROUNDING_ON } else { 0 })
    }
    pub fn blur_value(&self) -> Value {
        json!(self.blur)
    }
    pub fn animations_value(&self) -> Value {
        json!(self.animations)
    }
    pub fn bar_position_value(&self) -> Value {
        json!(self.bar_position.id())
    }

    pub fn app_count(&self) -> usize {
        self.apps.iter().filter(|on| **on).count()
    }
}

pub const MODE: &str = "mode";
pub const LAYOUT: &str = "general.layout";
pub const ROUNDING: &str = "decoration.rounding";
pub const BLUR: &str = "decoration.blur.enabled";
pub const ANIMATIONS: &str = "animations.enabled";
pub const BAR_POSITION: &str = "bar.position";

/// Every key the choice steps write, for the writer's allowlist.
pub const SEED_KEYS: [&str; 10] = [
    MODE,
    LAYOUT,
    "components.bar",
    "components.launcher",
    "components.notifications",
    "components.control-center",
    ROUNDING,
    BLUR,
    ANIMATIONS,
    BAR_POSITION,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_seed_the_table() {
        let std = Choices::for_profile(Profile::Standard);
        assert_eq!(std.mode, Mode::Hybrid);
        assert_eq!(std.tiling, Tiling::Radiant);
        assert_eq!(
            std.slots,
            ["hyperion", "eclipse-launcher", "eclipse-toasts", "eclipse-center"]
        );
        assert_eq!(std.app_count(), 0);

        let min = Choices::for_profile(Profile::Minimal);
        assert_eq!(min.mode, Mode::Wm);
        assert_eq!(min.slots, [NONE, "eclipse-launcher", NONE, NONE]);
        assert_eq!(min.app_count(), 0);

        let full = Choices::for_profile(Profile::Full);
        assert_eq!(full.mode, Mode::Hybrid);
        assert_eq!(full.slots, std.slots);
        assert_eq!(full.app_count(), FULL_APPS.len());

        // Agentic is Standard's set.
        assert_eq!(Choices::for_profile(Profile::Agentic), std);
    }

    #[test]
    fn slot_choices_end_in_none_and_start_with_the_native_one() {
        for s in &SLOTS {
            assert_eq!(*s.choices.last().unwrap(), NONE, "{}", s.key);
            assert_ne!(s.choices[0], NONE);
        }
    }

    #[test]
    fn candidates_skip_none_and_keep_slot_then_app_order() {
        let mut c = Choices::for_profile(Profile::Minimal);
        assert_eq!(c.candidates(), ["eclipse-launcher"]);
        c.slots[0] = "waybar";
        c.apps[0] = true;
        c.apps[8] = true;
        assert_eq!(
            c.candidates(),
            ["waybar", "eclipse-launcher", "app-browser", "app-bluetooth-ui"]
        );
    }

    #[test]
    fn seeds_are_exactly_the_writable_keys() {
        let keys: Vec<&str> = Choices::for_profile(Profile::Standard)
            .seeds()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, SEED_KEYS);
    }
}
