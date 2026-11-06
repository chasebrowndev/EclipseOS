// SPDX-License-Identifier: AGPL-3.0-only
//! The surfaces that float above windows: the menu and prompt sheet, and the
//! failure note.
//!
//! The icons live on a `Layer::Bottom` surface, so anything drawn inside it is
//! covered by every window that overlaps it. A context menu, the launch prompt
//! and the failure note are therefore each their own short-lived
//! `Layer::Overlay` surface, opened when there is something to show and
//! closed when there is not. (An `xdg_popup` would not do: a popup is stacked
//! with its parent's layer, so a Bottom surface's popups stay under windows.)
//!
//! This file is the pure half: [`plan`] compares the surface there is with the
//! one wanted and says what to open and close. `app::sync_floats` does it.

use iced::window::Id;

/// A floating surface, and the desktop surface (output) it floats over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Float {
    pub id: Id,
    pub over: Id,
}

/// What to do to bring one float in line with what is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    /// A float to take away.
    pub close: Option<Id>,
    /// Open a new one over this desktop surface.
    pub open: Option<Id>,
}

/// `have` is the float now open, `want` the desktop surface that should have
/// one (none: no float). A float over the right surface is left alone, so a
/// menu that merely moves does not flicker through a close and a reopen.
pub fn plan(have: Option<Float>, want: Option<Id>) -> Plan {
    match (have, want) {
        (Some(h), Some(w)) if h.over == w => Plan::default(),
        (Some(h), Some(w)) => Plan {
            close: Some(h.id),
            open: Some(w),
        },
        (Some(h), None) => Plan {
            close: Some(h.id),
            open: None,
        },
        (None, Some(w)) => Plan {
            close: None,
            open: Some(w),
        },
        (None, None) => Plan::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_open_and_nothing_wanted_is_nothing() {
        assert_eq!(plan(None, None), Plan::default());
    }

    #[test]
    fn a_wanted_float_is_opened_over_its_surface() {
        let s = Id::unique();
        assert_eq!(
            plan(None, Some(s)),
            Plan {
                close: None,
                open: Some(s)
            }
        );
    }

    #[test]
    fn an_unwanted_float_is_closed() {
        let f = Float {
            id: Id::unique(),
            over: Id::unique(),
        };
        assert_eq!(
            plan(Some(f), None),
            Plan {
                close: Some(f.id),
                open: None
            }
        );
    }

    #[test]
    fn a_float_over_the_right_surface_is_kept() {
        let f = Float {
            id: Id::unique(),
            over: Id::unique(),
        };
        assert_eq!(plan(Some(f), Some(f.over)), Plan::default());
    }

    #[test]
    fn a_float_over_the_wrong_surface_moves() {
        let f = Float {
            id: Id::unique(),
            over: Id::unique(),
        };
        let other = Id::unique();
        assert_eq!(
            plan(Some(f), Some(other)),
            Plan {
                close: Some(f.id),
                open: Some(other)
            }
        );
    }
}
