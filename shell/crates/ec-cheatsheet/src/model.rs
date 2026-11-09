// SPDX-License-Identifier: AGPL-3.0-only
//! What the sheet lists: every bind the live config holds, its chord in a
//! readable form, what it does, and which group it sits under.
//!
//! Pure data, no iced: the view draws what this module decides, and the tests
//! pin the chord spelling and the grouping.

use ec_abyss_config::input::{Action, Bind, Direction, Mods};
use xkbcommon::xkb::{keysym_get_name, Keysym};

/// The sections of the sheet, in the order they are drawn. The hero shows one
/// cell per group, so this is also the hero's reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Apps,
    Windows,
    Focus,
    Workspaces,
    Media,
    Session,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::Apps,
        Group::Windows,
        Group::Focus,
        Group::Workspaces,
        Group::Media,
        Group::Session,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Apps => "Apps",
            Group::Windows => "Windows",
            Group::Focus => "Focus & tile",
            Group::Workspaces => "Workspaces",
            Group::Media => "Media & devices",
            Group::Session => "Session & agents",
        }
    }
}

/// One line of the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub group: Group,
    pub label: String,
    /// The chord as keycap labels, modifiers first: `["Super", "Shift", "Q"]`.
    pub keys: Vec<String>,
    /// The config binds this chord to something other than the shipped
    /// default: a changed or added bind, as opposed to the stock table.
    pub custom: bool,
}

/// Every bind in `binds`, described, sorted by group and kept in config order
/// inside a group. `defaults` is the stock table the live config was merged
/// over; a bind matching one of its entries exactly is not `custom`.
pub fn entries(binds: &[Bind], defaults: &[Bind]) -> Vec<Entry> {
    let mut out: Vec<Entry> = binds
        .iter()
        .filter_map(|b| {
            let (group, label) = describe(&b.action)?;
            let custom = !defaults
                .iter()
                .any(|d| d.mods == b.mods && d.key == b.key && d.action == b.action);
            Some(Entry {
                group,
                label,
                keys: chord_keys(b.mods, b.key),
                custom,
            })
        })
        .collect();
    out.sort_by_key(|e| e.group);
    out
}

/// The chord as keycap labels. Modifiers in the order people type them.
pub fn chord_keys(mods: Mods, key: Keysym) -> Vec<String> {
    let mut keys = Vec::new();
    if mods.logo {
        keys.push("Super".to_owned());
    }
    if mods.ctrl {
        keys.push("Ctrl".to_owned());
    }
    if mods.alt {
        keys.push("Alt".to_owned());
    }
    if mods.shift {
        keys.push("Shift".to_owned());
    }
    keys.push(key_label(key));
    keys
}

/// One keysym as a keycap reads it: punctuation by its mark, a letter in
/// capitals, a word with its underscores spaced.
pub fn key_label(sym: Keysym) -> String {
    let name = keysym_get_name(sym);
    match name.as_str() {
        "slash" => "/".to_owned(),
        "backslash" => "\\".to_owned(),
        "comma" => ",".to_owned(),
        "period" => ".".to_owned(),
        "minus" => "-".to_owned(),
        "equal" => "=".to_owned(),
        "grave" => "`".to_owned(),
        "semicolon" => ";".to_owned(),
        "apostrophe" => "'".to_owned(),
        "bracketleft" => "[".to_owned(),
        "bracketright" => "]".to_owned(),
        // The keysyms' canonical names for the paging keys, which is what
        // xkb reports for Page_Up and Page_Down.
        "Prior" => "Page Up".to_owned(),
        "Next" => "Page Down".to_owned(),
        "asciitilde" => "~".to_owned(),
        "exclam" => "!".to_owned(),
        "at" => "@".to_owned(),
        "numbersign" => "#".to_owned(),
        "dollar" => "$".to_owned(),
        "percent" => "%".to_owned(),
        "asciicircum" => "^".to_owned(),
        "ampersand" => "&".to_owned(),
        "asterisk" => "*".to_owned(),
        "parenleft" => "(".to_owned(),
        "parenright" => ")".to_owned(),
        "underscore" => "_".to_owned(),
        "plus" => "+".to_owned(),
        "braceleft" => "{".to_owned(),
        "braceright" => "}".to_owned(),
        "bar" => "|".to_owned(),
        "colon" => ":".to_owned(),
        "quotedbl" => "\"".to_owned(),
        "less" => "<".to_owned(),
        "greater" => ">".to_owned(),
        "question" => "?".to_owned(),
        s if s.chars().count() == 1 => s.to_uppercase(),
        s => {
            let spaced = s.replace('_', " ");
            let mut chars = spaced.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => spaced,
            }
        }
    }
}

/// The group a bind belongs to and what it reads as, or `None` for an action
/// the input filter synthesises while a mode owns the seat. Those are not
/// bindable, so they have no line on the sheet.
///
/// The match is exhaustive on purpose: a new action is a new line here, not a
/// silent gap on the sheet.
fn describe(action: &Action) -> Option<(Group, String)> {
    use Group::*;
    let line = |g: Group, s: &str| Some((g, s.to_owned()));
    match action {
        Action::Spawn(cmd) => {
            let mut words = cmd.split_whitespace();
            // A script in `~/.local/bin` reads as its name, not its path.
            let program = words.next().unwrap_or("");
            let name = program.rsplit('/').next().unwrap_or(program);
            let group = match name {
                "wpctl" | "brightnessctl" | "playerctl" => Media,
                "loginctl" | "ec-ctl" | "systemctl" => Session,
                _ => Apps,
            };
            let shown = std::iter::once(name).chain(words).collect::<Vec<_>>().join(" ");
            line(group, &format!("Run {}", shorten(&shown, 56)))
        }
        Action::Close => line(Windows, "Close window"),
        Action::ToggleFloating => line(Windows, "Float or tile window"),
        Action::ToggleFullscreen => line(Windows, "Fullscreen"),
        Action::ToggleMaximize => line(Windows, "Maximize or restore"),
        Action::Minimize => line(Windows, "Send window away"),
        Action::Unminimize => line(Windows, "Bring back last window"),
        Action::Focus(d) => line(Focus, &format!("Focus {}", dir(*d))),
        Action::Move(d) => line(Focus, &format!("Swap window {}", dir(*d))),
        Action::Tile(d) => line(Focus, &format!("Quick tile {}", dir(*d))),
        Action::Switcher(false) => line(Focus, "Window switcher"),
        Action::Switcher(true) => line(Focus, "Window switcher, backwards"),
        Action::ToggleLayout => line(Focus, "Toggle layout"),
        Action::Priority(n) if *n >= 0 => line(Focus, "Raise window priority"),
        Action::Priority(_) => line(Focus, "Lower window priority"),
        Action::SwitchWorkspace(n) => line(Workspaces, &format!("Workspace {n}")),
        Action::WorkspaceNext => line(Workspaces, "Next workspace"),
        Action::WorkspacePrev => line(Workspaces, "Previous workspace"),
        Action::MoveToWorkspace(n) => line(Workspaces, &format!("Move window to workspace {n}")),
        Action::MoveToOutputWorkspace(n) => line(Workspaces, &format!("Move window to display {n}")),
        Action::DesktopSwitch(d) => line(Workspaces, &format!("Desktop {}", dir(*d))),
        Action::DesktopMove(d) => line(Workspaces, &format!("Move window to desktop {}", dir(*d))),
        Action::ToggleOverview => line(Workspaces, "Overview"),
        Action::Quit => line(Session, "Quit the compositor"),
        Action::SwitchVt(n) => line(Session, &format!("Switch to VT {n}")),
        Action::AgentOverride => line(Session, "Pause every agent"),
        Action::AgentTerminate => line(Session, "Stop every agent"),
        Action::AgentAttention => line(Session, "Decisions waiting"),
        Action::ScreenshotFull => line(Session, "Screenshot"),
        Action::ScreenshotRegion => line(Session, "Screenshot: select region"),
        Action::AnnotationSelect => line(Session, "Annotate: select region"),
        Action::AnnotationDismiss => line(Session, "Annotate: dismiss"),
        Action::AnnotationExpand => line(Session, "Annotate: expand"),
        Action::AnnotationAutoToggle => line(Session, "Annotate: auto mode"),
        Action::AnnotationModelSwitch => line(Session, "Annotate: switch model"),
        Action::AnnotationImageToggle => line(Session, "Annotate: image toggle"),
        Action::Calibrate(_)
        | Action::RegionSelect(_)
        | Action::Prompt(_)
        | Action::WindowMenu(_)
        | Action::Overlay(_, _) => None,
    }
}

fn dir(d: Direction) -> &'static str {
    match d {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let head: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{head}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sup() -> Mods {
        Mods {
            logo: true,
            ..Mods::default()
        }
    }

    #[test]
    fn a_chord_reads_modifiers_first_in_typing_order() {
        let mods = Mods {
            logo: true,
            shift: true,
            ..Mods::default()
        };
        assert_eq!(
            chord_keys(mods, Keysym::q),
            ["Super", "Shift", "Q"].map(str::to_owned)
        );
    }

    #[test]
    fn a_bare_key_has_no_modifier_and_punctuation_is_its_mark() {
        assert_eq!(
            chord_keys(Mods::default(), Keysym::slash),
            ["/"].map(str::to_owned)
        );
        assert_eq!(
            chord_keys(sup(), Keysym::Page_Up),
            ["Super", "Page Up"].map(str::to_owned)
        );
    }

    #[test]
    fn spawned_media_keys_group_as_devices_and_apps_as_apps() {
        let media = describe(&Action::Spawn(
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle".into(),
        ));
        assert_eq!(media.map(|(g, _)| g), Some(Group::Media));
        let app = describe(&Action::Spawn("foot".into()));
        assert_eq!(app, Some((Group::Apps, "Run foot".to_owned())));
    }

    #[test]
    fn a_script_path_reads_as_its_name_and_shifted_punctuation_as_its_mark() {
        let script = describe(&Action::Spawn("/home/u/.local/bin/screenshot-region".into()));
        assert_eq!(script, Some((Group::Apps, "Run screenshot-region".to_owned())));
        let tilde = Mods {
            logo: true,
            shift: true,
            ..Mods::default()
        };
        assert_eq!(
            chord_keys(tilde, Keysym::asciitilde),
            ["Super", "Shift", "~"].map(str::to_owned)
        );
    }

    #[test]
    fn a_synthesised_action_has_no_line() {
        assert_eq!(describe(&Action::Prompt(Keysym::Escape)), None);
    }

    #[test]
    fn a_bind_matching_a_default_is_not_custom() {
        let stock = Bind {
            mods: sup(),
            key: Keysym::q,
            action: Action::Spawn("foot".into()),
        };
        let changed = Bind {
            mods: sup(),
            key: Keysym::q,
            action: Action::Spawn("kitty".into()),
        };
        let listed = entries(&[stock.clone(), changed], &[stock]);
        assert_eq!(listed.iter().filter(|e| e.custom).count(), 1);
        assert!(listed.iter().any(|e| e.label == "Run kitty" && e.custom));
    }

    #[test]
    fn entries_sort_by_group_and_keep_config_order_within() {
        let focus = Bind {
            mods: sup(),
            key: Keysym::h,
            action: Action::Focus(Direction::Left),
        };
        let terminal = Bind {
            mods: sup(),
            key: Keysym::Return,
            action: Action::Spawn("foot".into()),
        };
        let out = entries(&[focus, terminal], &[]);
        assert_eq!(out[0].group, Group::Apps);
        assert_eq!(out[1].group, Group::Focus);
        assert_eq!(out[1].label, "Focus left");
        assert!(out[0].custom && out[1].custom);
    }
}
