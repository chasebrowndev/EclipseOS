// SPDX-License-Identifier: AGPL-3.0-only
//! Built-in key, gesture and mouse bindings and the merge of config binds over
//! them (COMP-13 §1.1, COMP-04 §6).

use super::*;

/// Merge config binds over the built-in defaults (COMP-13 §1.1).
///
/// Config binds *extend* the defaults; they never replace the table. A config
/// bind naming the same `(mods, key)` as a default replaces that default in
/// place, and a later config file replaces an earlier one the same way — the
/// result holds exactly one entry per chord, so `action_for`'s first-match
/// lookup cannot be shadowed by a default sitting ahead of a user bind.
///
/// `Super+Escape`, `Super+Shift+Escape` (COMP-04 §6) and `Super+space` (COMP-13 §1.1) can never be
/// removed here: `parse_bind` refuses to produce them, so no config entry can
/// match those defaults and the defaults always survive the merge.
pub(crate) fn merge_binds(defaults: Vec<Bind>, from_file: Vec<Bind>) -> Vec<Bind> {
    let mut out: Vec<Bind> = Vec::with_capacity(defaults.len() + from_file.len());
    for b in from_file {
        match out.iter_mut().find(|o| o.mods == b.mods && o.key == b.key) {
            Some(slot) => *slot = b,
            None => out.push(b),
        }
    }
    for d in defaults {
        if !out.iter().any(|o| o.mods == d.mods && o.key == d.key) {
            out.push(d);
        }
    }
    out
}

/// The built-in binds, with the launcher's chords taken from
/// `launcher.bind` in place of the shipped Super+E / Super+R. They are still
/// defaults: a `bind` block on the same chord wins over them, and they win
/// over any other built-in on the chord the human picked (first default in
/// the list wins in `merge_binds`).
pub fn defaults_with_launcher(launcher: &Launcher) -> Vec<Bind> {
    let spawn = || Action::Spawn("ec-launcher".into());
    let mut out: Vec<Bind> = [&launcher.bind.open, &launcher.bind.run]
        .into_iter()
        .filter_map(|c| c.chord)
        .map(|(mods, key)| Bind {
            mods,
            key,
            action: spawn(),
        })
        .collect();
    out.extend(default_binds().into_iter().filter(|b| b.action != spawn()));
    out
}

/// The owner's Hyprland workspace swipe: three fingers moving left bring in
/// the next workspace, the way dragging a page left reveals the one after it.
pub fn default_gesture_binds() -> Vec<GestureBind> {
    vec![
        GestureBind {
            fingers: 3,
            direction: Direction::Left,
            action: Action::WorkspaceNext,
        },
        GestureBind {
            fingers: 3,
            direction: Direction::Right,
            action: Action::WorkspacePrev,
        },
    ]
}

/// Super + two-finger touchpad drag moves the window under the pointer
/// (ADR 0059). Super, not Alt: two-finger scroll with Alt held is a common
/// app gesture (zoom, horizontal scroll), and Super reaches no app.
pub fn default_drag_gestures() -> Vec<DragGesture> {
    vec![DragGesture {
        fingers: 2,
        mods: Mods {
            logo: true,
            ..Mods::default()
        },
    }]
}

/// Hyprland's `bindm` pair on Alt (ADR 0057): hold Alt, drag with the left
/// button to move a window and with the right button to resize it. Alt rather
/// than Super, which is kept free for a future launcher.
pub fn default_mouse_binds() -> Vec<MouseBind> {
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    vec![
        MouseBind {
            mods: alt,
            button: MouseButton::Left,
            action: MouseAction::MoveWindow,
        },
        MouseBind {
            mods: alt,
            button: MouseButton::Right,
            action: MouseAction::ResizeWindow,
        },
    ]
}

pub(crate) fn m(logo: bool, shift: bool, ctrl: bool, alt: bool) -> Mods {
    Mods {
        logo,
        shift,
        ctrl,
        alt,
    }
}

/// The owner's Hyprland binds, key for key, so moving between the two
/// compositors costs no relearning (`~/.config/hypr/hyprland.lua`). Where
/// Hyprland spawns a Quickshell popup the equivalent here spawns our own
/// binary; where abyss has no matching action at all — fullscreen, pseudo,
/// the special workspace, the keybind cheatsheet, the dashboard, the overview
/// — the bind is simply absent rather than approximated.
///
/// `Super+Escape` is bound here and nowhere else: it is the trusted-UI
/// override chord (COMP-04 §6), always present and not rebindable —
/// `parse_bind` refuses any config that names it.
pub fn default_binds() -> Vec<Bind> {
    let sup = m(true, false, false, false);
    let sup_shift = m(true, true, false, false);
    let sup_ctrl = m(true, false, true, false);
    let none = m(false, false, false, false);
    let mut b = vec![
        Bind {
            mods: sup,
            key: Keysym::Escape,
            action: Action::AgentOverride,
        },
        // The second reserved chord (COMP-04 §6): pause and terminate.
        Bind {
            mods: sup_shift,
            key: Keysym::Escape,
            action: Action::AgentTerminate,
        },
        Bind {
            mods: sup,
            key: Keysym::space,
            action: Action::AgentAttention,
        },
        // Applications. Every shipped bind names a binary EclipseOS installs:
        // a default pointing at something that is not there spawns, dies
        // silently, and reads to the user as a dead keybind (this is exactly
        // what kitty, dolphin and firefox did on the first real install).
        // Terminal is Super+Q and Super+Return both, the two chords people
        // reach for; anything else belongs in the user's own config.
        Bind {
            mods: sup,
            key: Keysym::q,
            action: Action::Spawn("foot".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::Return,
            action: Action::Spawn("foot".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::e,
            action: Action::Spawn("ec-launcher".into()),
        },
        // The agent console (A-08 §9): an ordinary, rebindable default, since
        // opening a client grants nothing. Layouts where `/` is shifted need
        // Super+Shift+<that key> (docs/CONFIG.md).
        Bind {
            mods: sup,
            key: Keysym::slash,
            action: Action::Spawn("ec-console".into()),
        },
        // The desktop's own surfaces. Hyprland reaches these through
        // `qs -c eclipse ipc call ui toggle ...`; ours are separate binaries,
        // and each exits on Escape, so a second press of the bind is not a
        // toggle. Only the two that exist are bound.
        Bind {
            mods: sup,
            key: Keysym::r,
            action: Action::Spawn("ec-launcher".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::n,
            action: Action::Spawn("ec-center".into()),
        },
        // Windows.
        Bind {
            mods: sup,
            key: Keysym::c,
            action: Action::Close,
        },
        Bind {
            mods: sup,
            key: Keysym::v,
            action: Action::ToggleFloating,
        },
        // Minimize is a pair, not a toggle: a window that has been sent away
        // holds no focus, so there is nothing for the same chord to act on.
        Bind {
            mods: sup,
            key: Keysym::h,
            action: Action::Minimize,
        },
        Bind {
            mods: sup_shift,
            key: Keysym::H,
            action: Action::Unminimize,
        },
        Bind {
            mods: sup,
            key: Keysym::j,
            action: Action::ToggleLayout,
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Q,
            action: Action::Quit,
        },
        // Focus. Hyprland binds the arrows and nothing else, so neither do we.
        Bind {
            mods: sup,
            key: Keysym::Left,
            action: Action::Focus(Direction::Left),
        },
        Bind {
            mods: sup,
            key: Keysym::Right,
            action: Action::Focus(Direction::Right),
        },
        Bind {
            mods: sup,
            key: Keysym::Up,
            action: Action::Focus(Direction::Up),
        },
        Bind {
            mods: sup,
            key: Keysym::Down,
            action: Action::Focus(Direction::Down),
        },
        // Swapping a tiled window with its neighbour has no Hyprland bind to
        // copy; Shift over the focus arrows is the obvious pair and collides
        // with nothing. Floating windows move by mouse drag (`mousebind`).
        // Vertically, Shift+Up/Down change the window's Radiant priority
        // instead: a drag does the vertical swap, and `move-up`/`move-down`
        // stay bindable.
        Bind {
            mods: sup_shift,
            key: Keysym::Left,
            action: Action::Move(Direction::Left),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Right,
            action: Action::Move(Direction::Right),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Up,
            action: Action::Priority(1),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Down,
            action: Action::Priority(-1),
        },
        // Session.
        Bind {
            mods: sup_shift,
            key: Keysym::L,
            action: Action::Spawn("loginctl lock-session".into()),
        },
        // Media and brightness keys, unmodified, exactly as Hyprland has them.
        Bind {
            mods: none,
            key: Keysym::XF86_AudioRaiseVolume,
            action: Action::Spawn("wpctl set-volume -l 1 @DEFAULT_AUDIO_SINK@ 5%+".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioLowerVolume,
            action: Action::Spawn("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioMute,
            action: Action::Spawn("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioMicMute,
            action: Action::Spawn("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_MonBrightnessUp,
            action: Action::Spawn("brightnessctl -e4 -n2 set 5%+".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_MonBrightnessDown,
            action: Action::Spawn("brightnessctl -e4 -n2 set 5%-".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioNext,
            action: Action::Spawn("playerctl next".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioPrev,
            action: Action::Spawn("playerctl previous".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioPlay,
            action: Action::Spawn("playerctl play-pause".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioPause,
            action: Action::Spawn("playerctl play-pause".into()),
        },
    ];
    const DIGITS: [Keysym; 10] = [
        Keysym::_1,
        Keysym::_2,
        Keysym::_3,
        Keysym::_4,
        Keysym::_5,
        Keysym::_6,
        Keysym::_7,
        Keysym::_8,
        Keysym::_9,
        Keysym::_0,
    ];
    // Shifted digits on a US layout; both forms are accepted so the bind fires
    // whether or not the client layout shifts the symbol.
    const SHIFTED: [Keysym; 10] = [
        Keysym::exclam,
        Keysym::at,
        Keysym::numbersign,
        Keysym::dollar,
        Keysym::percent,
        Keysym::asciicircum,
        Keysym::ampersand,
        Keysym::asterisk,
        Keysym::parenleft,
        Keysym::parenright,
    ];
    for (i, key) in DIGITS.iter().enumerate() {
        b.push(Bind {
            mods: sup,
            key: *key,
            action: Action::SwitchWorkspace(i + 1),
        });
        b.push(Bind {
            mods: sup_shift,
            key: *key,
            action: Action::MoveToWorkspace(i + 1),
        });
        b.push(Bind {
            mods: sup_shift,
            key: SHIFTED[i],
            action: Action::MoveToWorkspace(i + 1),
        });
        // Ctrl+Super+[1-9,0] (ADR 0049): move the focused window to display
        // (i + 1)'s currently active workspace. `DIGITS[9]` is the `0` key,
        // giving display 10, the same "0 wraps to the tenth slot" convention
        // `MoveToWorkspace` already uses above.
        b.push(Bind {
            mods: sup_ctrl,
            key: *key,
            action: Action::MoveToOutputWorkspace((i + 1) as u8),
        });
    }
    b
}
