// SPDX-License-Identifier: AGPL-3.0-only
//! Task grouping under `mode "de"` (COMP-17 §2): the pure half.
//!
//! Plasma's task manager shows one chip per application, not per window. The
//! bar does the same in `de` and only in `de`: `wm` has no chips and `hybrid`
//! keeps one chip per window, exactly as before.
//!
//! A group is the windows of one application on the strip being drawn (this
//! output, this desktop). Its chip is *represented* by one of them — the
//! member with the lowest handle, which does not change as focus moves
//! around inside the group, so the chip's animation is never restarted by a
//! click. Everything that is the group's and not the representative's —
//! the count, whether any member has focus, whether every member is away —
//! is a [`Tally`].
//!
//! Nothing here draws or touches the compositor.

use crate::model::{Trust, Window};

/// The windows one chip stands for.
#[derive(Debug, Clone)]
pub struct Group<'a> {
    /// The window the chip is keyed and drawn by.
    pub rep: &'a Window,
    /// Every window of the group, in the compositor's order. Empty for an
    /// idle pinned chip, which has no window at all.
    pub members: Vec<&'a Window>,
}

/// What a chip says about its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// How many windows. `1` for an ordinary chip, `0` for an idle pin.
    pub count: usize,
    /// Any member holds the keyboard.
    pub focused: bool,
    /// Every member is put away. An idle pin counts as away, like its chip.
    pub minimized: bool,
}

impl Tally {
    /// A chip that is one window.
    pub fn solo(w: &Window, focused: bool) -> Tally {
        Tally {
            count: usize::from(!crate::pins::is_idle(w)),
            focused,
            minimized: w.minimized,
        }
    }

    /// Whether a click opens a list rather than acting on a window.
    pub fn is_group(&self) -> bool {
        self.count > 1
    }
}

/// The key windows are grouped by: the app id, case-folded (the same match
/// pinning makes). `None` for a window that never joins a group: one with no
/// app id to group by, and a `secret` window — its client-controlled strings
/// are not even compared.
pub fn key_of(w: &Window) -> Option<String> {
    if w.trust == Trust::Secret || w.app_id.is_empty() {
        return None;
    }
    Some(w.app_id.to_lowercase())
}

/// Group `windows`, in the order each group is first seen.
pub fn group(windows: Vec<&Window>) -> Vec<Group<'_>> {
    let mut out: Vec<(Option<String>, Group<'_>)> = Vec::with_capacity(windows.len());
    for w in windows {
        let key = key_of(w);
        if let Some(k) = key.as_deref() {
            if let Some((_, g)) = out.iter_mut().find(|(gk, _)| gk.as_deref() == Some(k)) {
                if w.handle < g.rep.handle {
                    g.rep = w;
                }
                g.members.push(w);
                continue;
            }
        }
        out.push((
            key,
            Group {
                rep: w,
                members: vec![w],
            },
        ));
    }
    out.into_iter().map(|(_, g)| g).collect()
}

impl Group<'_> {
    /// The group's reading. `focused` says whether a window holds focus (the
    /// bar's authoritative answer, not the per-window flag).
    pub fn tally(&self, focused: impl Fn(&Window) -> bool) -> Tally {
        if self.members.is_empty() {
            return Tally::solo(self.rep, false);
        }
        Tally {
            count: self.members.len(),
            focused: self.members.iter().any(|w| focused(w) && !w.minimized),
            minimized: self.members.iter().all(|w| w.minimized),
        }
    }

    /// The window a right-click on the chip acts on: the one with focus, else
    /// the representative.
    pub fn menu_target(&self, focused: impl Fn(&Window) -> bool) -> u64 {
        self.members
            .iter()
            .find(|w| focused(w))
            .map_or(self.rep.handle, |w| w.handle)
    }
}

/// What a left click on a chip does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// List the group's windows (`de`, several of them).
    Open(u64),
    /// Bring the window forward: it is up but not the one with focus.
    Focus(u64),
    /// Send the window away, or bring it back.
    Toggle(u64),
}

/// The chip's click. A group of one is an ordinary chip, exactly as under
/// `hybrid`: a click on the focused window sends it away, on one that is away
/// brings it back, and on one that is up but behind brings it forward.
pub fn click(de: bool, t: &Tally, handle: u64) -> Click {
    if de && t.is_group() {
        Click::Open(handle)
    } else if !t.minimized && !t.focused {
        Click::Focus(handle)
    } else {
        Click::Toggle(handle)
    }
}

/// What a chip of `count` windows reads: the application, then the count.
/// The group has no one title, so it has one rung of words; the title of the
/// representative must never stand for the others.
pub fn chip_text(w: &Window, count: usize) -> String {
    if count > 1 {
        format!("{} \u{b7} {count}", w.name())
    } else {
        w.name().to_owned()
    }
}

/// The row of a window in a group's list: what it says, and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub handle: u64,
    /// `Window::label`, so a `secret` window's title is its placeholder.
    pub title: String,
    pub focused: bool,
    pub minimized: bool,
}

/// The list a group chip opens.
pub fn rows(g: &Group<'_>, focused: impl Fn(&Window) -> bool) -> Vec<Row> {
    g.members
        .iter()
        .map(|w| Row {
            handle: w.handle,
            title: w.label().to_owned(),
            focused: focused(w) && !w.minimized,
            minimized: w.minimized,
        })
        .collect()
}

/// What a click on a row does to its window: bring forward a window that is
/// up but behind, bring back one that is away, and leave the focused one be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activate {
    Focus(u64),
    Restore(u64),
    Nothing,
}

pub fn activation(row: &Row) -> Activate {
    if row.minimized {
        Activate::Restore(row.handle)
    } else if row.focused {
        Activate::Nothing
    } else {
        Activate::Focus(row.handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pins::Pinned;

    fn win(handle: u64, app_id: &str, title: &str) -> Window {
        let mut w = Pinned::new("x", "t").idle;
        w.handle = handle;
        w.app_id = app_id.to_owned();
        w.title = title.to_owned();
        w.minimized = false;
        w
    }

    #[test]
    fn windows_of_one_app_share_a_chip_in_order_of_first_appearance() {
        let (a, b, c, d) = (
            win(4, "foot", "one"),
            win(2, "firefox", "web"),
            win(7, "Foot", "two"),
            win(9, "kitty", "k"),
        );
        let g = group(vec![&a, &b, &c, &d]);
        let reps: Vec<u64> = g.iter().map(|g| g.rep.handle).collect();
        // foot (4 and 7, case-folded) leads, represented by its lowest handle.
        assert_eq!(reps, [4, 2, 9]);
        assert_eq!(g[0].members.iter().map(|w| w.handle).collect::<Vec<_>>(), [4, 7]);
        assert_eq!(g[1].members.len(), 1);
    }

    #[test]
    fn the_representative_is_the_lowest_handle_whatever_the_order() {
        let (a, b) = (win(8, "foot", "x"), win(3, "foot", "y"));
        let g = group(vec![&a, &b]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].rep.handle, 3);
        // Members keep the compositor's order for the list.
        assert_eq!(g[0].members[0].handle, 8);
    }

    #[test]
    fn a_window_with_no_app_id_or_a_secret_one_is_never_grouped() {
        let (a, b) = (win(1, "", "a"), win(2, "", "b"));
        assert_eq!(group(vec![&a, &b]).len(), 2);
        let mut s1 = win(3, "vault", "pw");
        s1.trust = Trust::Secret;
        let s2 = win(4, "vault", "other");
        assert_eq!(group(vec![&s1, &s2]).len(), 2);
    }

    #[test]
    fn a_tally_counts_focus_and_away_for_the_whole_group() {
        let mut a = win(1, "foot", "a");
        let mut b = win(2, "foot", "b");
        let g = group(vec![&a, &b]);
        let t = g.first().expect("group").tally(|w| w.handle == 2);
        assert_eq!(
            t,
            Tally {
                count: 2,
                focused: true,
                minimized: false
            }
        );
        assert!(t.is_group());
        // All away: the chip is the dimmed one, and nothing is focused.
        a.minimized = true;
        b.minimized = true;
        let g = group(vec![&a, &b]);
        let t = g[0].tally(|w| w.handle == 2);
        assert!(t.minimized && !t.focused);
    }

    #[test]
    fn a_lone_window_is_not_a_group() {
        let a = win(1, "foot", "a");
        let g = group(vec![&a]);
        assert!(!g[0].tally(|_| false).is_group());
    }

    #[test]
    fn an_idle_pin_has_no_window_to_count() {
        let p = Pinned::new("steam", "Steam");
        let t = Tally::solo(&p.idle, false);
        assert_eq!(t.count, 0);
        assert!(!t.is_group());
    }

    #[test]
    fn the_chip_names_the_app_and_counts_never_the_title() {
        let a = win(1, "foot", "~/secret/plans");
        assert_eq!(chip_text(&a, 3), "foot \u{b7} 3");
        assert_eq!(chip_text(&a, 1), "foot");
    }

    #[test]
    fn a_secret_windows_title_never_reaches_the_list() {
        let mut s = win(5, "vault", "hunter2 — passwords");
        s.trust = Trust::Secret;
        let a = win(6, "vault", "notes");
        let g = group(vec![&s, &a]);
        // Secret windows stay out of groups, so each lists on its own; the
        // row text is still the placeholder wherever it is built.
        let rows = rows(&g[0], |_| false);
        assert_eq!(rows[0].title, "Protected window");
        assert!(!rows[0].title.contains("hunter2"));
    }

    #[test]
    fn the_list_lights_the_focused_row_and_a_click_picks_the_right_verb() {
        let mut a = win(1, "foot", "a");
        let b = win(2, "foot", "b");
        let c = win(3, "foot", "c");
        a.minimized = true;
        let g = group(vec![&a, &b, &c]);
        let r = rows(&g[0], |w| w.handle == 2);
        assert_eq!(activation(&r[0]), Activate::Restore(1));
        assert_eq!(activation(&r[1]), Activate::Nothing);
        assert_eq!(activation(&r[2]), Activate::Focus(3));
    }

    #[test]
    fn a_click_on_a_lone_window_acts_as_it_always_did() {
        let a = win(1, "foot", "a");
        let solo = |focused: bool, minimized: bool| Tally {
            count: 1,
            focused,
            minimized,
        };
        let _ = &a;
        // Behind another window: forward. Focused: away. Away: back.
        assert_eq!(click(true, &solo(false, false), 1), Click::Focus(1));
        assert_eq!(click(true, &solo(true, false), 1), Click::Toggle(1));
        assert_eq!(click(true, &solo(false, true), 1), Click::Toggle(1));
    }

    #[test]
    fn a_click_on_a_group_opens_its_list_only_under_de() {
        let t = Tally {
            count: 3,
            focused: false,
            minimized: false,
        };
        assert_eq!(click(true, &t, 5), Click::Open(5));
        // Hybrid never has a group, but were it handed one it would act as a
        // window: grouping is a `de` behaviour and nothing else opens a list.
        assert_eq!(click(false, &t, 5), Click::Focus(5));
    }

    #[test]
    fn a_right_click_on_the_chip_acts_on_the_focused_member_else_the_lowest() {
        let (a, b) = (win(1, "foot", "a"), win(2, "foot", "b"));
        let g = group(vec![&a, &b]);
        assert_eq!(g[0].menu_target(|w| w.handle == 2), 2);
        assert_eq!(g[0].menu_target(|_| false), 1);
    }
}
