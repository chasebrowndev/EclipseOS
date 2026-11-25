// SPDX-License-Identifier: AGPL-3.0-only
//! How the greeting, the account list, the password field and the power pills
//! are sized inside their tile: the rest of the tile kinds' half of the shared
//! rules that [`crate::tile`] starts with the clock and the Glances.
//!
//! The same contract as `tile`: pure, no iced, no I/O. Every plan takes the
//! tile's *inner* box ([`crate::tile::inner`]) in logical px of the real
//! screen and returns plain data. The greeter turns it into widgets; the
//! layout builder's preview draws it scaled by its own factor. Every number a
//! tile of these kinds is drawn with lives here (or in `tile`), so a change to
//! the screen is a change to the preview.
//!
//! Plans never exceed the box they are given (a size is a hint; a tile never
//! gets a size it cannot hold), for any input including zero and NaN.

use crate::tile::{fit_in, Inner};

// ---- shapes ----------------------------------------------------------------

/// The card's corner radius (STYLE.md: 22-24px on the login screen). Glass
/// panes are clamped to half their short side, so a small card stays round.
pub const CARD_RADIUS: f32 = 22.0;
/// A cell inside a card: a user row, the password field.
pub const CELL_RADIUS: f32 = 14.0;
/// Asks for a pill: clamped to half the cell's short side.
pub const PILL_RADIUS: f32 = 999.0;
/// The radius a shape of `short` px on its short side is drawn with: its ask,
/// clamped to half of it.
pub fn radius(ask: f32, short: f32) -> f32 {
    ask.min(short / 2.0).max(0.0)
}

/// The frosted look of a pane: ink laid over what is behind it, as `(fill,
/// rim)` alphas of the text colour. A card first, then a cell inside it. How far
/// a card's backing is darkened before the frost shows through is [`CARD_BACK`].
pub const CARD: (f32, f32) = (0.035, 0.18);
pub const CARD_BACK: f32 = 0.30;
pub const CELL: (f32, f32) = (0.045, 0.10);

/// A control that cannot be used yet: its ground and its ink, as a share.
pub const DISABLED: f32 = 0.5;

// ---- greeting ----------------------------------------------------------------

/// The greeting line: the smaller of a height share and a width share, clamped.
pub const GREETING_H: f32 = 0.62;
pub const GREETING_W: f32 = 0.07;
pub const GREETING_MIN: f32 = 20.0;
pub const GREETING_MAX: f32 = 44.0;

// ---- inside a card -------------------------------------------------------------

pub const FIELD_MIN: f32 = 36.0;
pub const FIELD_MAX: f32 = 56.0;
/// The line under the field: what is happening.
pub const NOTE_H: f32 = 18.0;
pub const NOTE_GAP: f32 = 10.0;
/// The Users header row.
pub const HEAD_H: f32 = 16.0;
pub const ROW_MIN: f32 = 36.0;
pub const ROW_MAX: f32 = 64.0;
pub const ROW_GAP: f32 = 8.0;
pub const ROW_PAD_X: f32 = 14.0;
/// The gap under the Users header, and between a disc and its name or the
/// field and its pill.
pub const LINE_GAP: f32 = 11.0;
/// An account's disc: a share of its row, clamped; its letter a share of it.
pub const AVATAR_FRAC: f32 = 0.7;
pub const AVATAR_MIN: f32 = 22.0;
pub const AVATAR_MAX: f32 = 44.0;
pub const INITIAL_FRAC: f32 = 0.45;
/// The typed length: dots, then a caret.
pub const DOT: f32 = 8.0;
pub const DOT_GAP: f32 = 7.0;
pub const DOTS_MAX: usize = 24;
pub const CARET_W: f32 = 2.0;
pub const CARET_H: f32 = 20.0;
/// The Unlock pill beside the field: a share of the tile, clamped.
pub const SUBMIT_W_FRAC: f32 = 0.3;
pub const SUBMIT_MIN: f32 = 72.0;
pub const SUBMIT_MAX: f32 = 140.0;
/// The inner width at which the three power pills carry their full labels.
pub const POWER_FULL_W: f32 = 300.0;

// ---- words ---------------------------------------------------------------------

pub const ACCOUNTS_HEAD: &str = "ACCOUNTS";
pub const NO_ACCOUNTS: &str = "No accounts";
pub const PLACEHOLDER: &str = "Password";
/// The greeter's button beside the field.
pub const SIGN_IN: &str = "Sign in";
/// The note under the field before an account is chosen: how the screen starts.
pub const NOTE_IDLE: &str = "Choose an account to sign in";
/// The power pills, `(full label, short label)`, in order: suspend, restart,
/// power off. The short ones are for a tile too narrow for three full ones.
pub const POWER: [(&str, &str); 3] = [("Suspend", "Sleep"), ("Restart", "Restart"), ("Power off", "Off")];

/// What the list says when accounts did not fit.
pub fn more_label(more: usize) -> String {
    format!("+{more} more")
}

// ---- greeting plan ---------------------------------------------------------------

/// The greeting's size, px: one line, centred vertically, never wrapped.
pub fn greeting(inner: Inner) -> f32 {
    fit_in(
        (inner.h * GREETING_H).min(inner.w * GREETING_W),
        GREETING_MIN,
        GREETING_MAX,
    )
}

// ---- users plan -------------------------------------------------------------------

/// What the account list draws: the header, then `shown` rows of `row_h`, then
/// a "+N more" line when `more > 0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsersPlan {
    pub shown: usize,
    pub more: usize,
    pub row_h: f32,
    /// The disc's diameter; its letter is `avatar * INITIAL_FRAC`.
    pub avatar: f32,
}

impl UsersPlan {
    /// The height the list takes, header included; never above the inner height
    /// when the tile can hold the header.
    pub fn used_h(&self) -> f32 {
        let rows = (self.shown + usize::from(self.more > 0)) as f32;
        HEAD_H + LINE_GAP + rows * self.row_h + (rows - 1.0).max(0.0) * ROW_GAP
    }
}

/// The account list for `n` accounts: as many comfortable rows as fit, the last
/// slot given to "+N more" when some do not.
pub fn users(inner: Inner, n: usize) -> UsersPlan {
    let avail = (inner.h - HEAD_H - LINE_GAP).max(0.0);
    let fit_rows = (((avail + ROW_GAP) / (ROW_MIN + ROW_GAP)).floor().max(1.0)) as usize;
    let shown = if n > fit_rows {
        fit_rows.saturating_sub(1).max(1)
    } else {
        n
    };
    let more = n.saturating_sub(shown);
    let total = (shown + usize::from(more > 0)).max(1) as f32;
    let row_h = ((avail - ROW_GAP * (total - 1.0)) / total).clamp(0.0, ROW_MAX);
    let avatar = fit_in(row_h * AVATAR_FRAC, AVATAR_MIN, AVATAR_MAX).min(row_h);
    UsersPlan {
        shown,
        more,
        row_h,
        avatar,
    }
}

// ---- password plan ------------------------------------------------------------------

/// The field row: the field (filling) and the Unlock pill, both `field_h` tall,
/// with the note beneath.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PasswordPlan {
    pub field_h: f32,
    pub submit_w: f32,
}

pub fn password(inner: Inner) -> PasswordPlan {
    PasswordPlan {
        field_h: fit_in(inner.h - NOTE_H - NOTE_GAP, FIELD_MIN, FIELD_MAX),
        submit_w: fit_in(inner.w * SUBMIT_W_FRAC, SUBMIT_MIN, SUBMIT_MAX),
    }
}

// ---- power plan --------------------------------------------------------------------

/// Three equal pills in a row, centred vertically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerPlan {
    pub pill_h: f32,
    /// Full labels, else the short ones.
    pub full: bool,
}

pub fn power(inner: Inner) -> PowerPlan {
    PowerPlan {
        pill_h: fit_in(inner.h, 0.0, FIELD_MAX),
        full: inner.w >= POWER_FULL_W,
    }
}

// ---- the lock screen ------------------------------------------------------------
//
// The lock draws the same layout engine's tiles but not the greeter's panes:
// the clock is bare type, the greeting is a pill holding the account, the
// password is a field with its note and key hints. Same contract: the lock
// view and the layout builder's lock preview both ask these.

/// The lock's own sizes inside a pane.
pub mod lock {
    pub const FIELD_MIN: f32 = 44.0;
    /// Below this the Unlock pill trims its padding to stay inside the field.
    pub const FIELD_ROOMY: f32 = 52.0;
    pub const UNLOCK_PAD_TIGHT: f32 = 2.0;
    pub const FIELD_MAX: f32 = 56.0;
    /// The line under the field: what is happening.
    pub const NOTE_H: f32 = 18.0;
    pub const NOTE_GAP: f32 = 6.0;
    /// The hint line under the note, when the pane has the room.
    pub const HINT_H: f32 = 22.0;
    /// The account disc in the greeting pill: a share of the pill, clamped.
    pub const AVATAR_FRAC: f32 = 0.9;
    pub const AVATAR_MIN: f32 = 22.0;
    pub const AVATAR_MAX: f32 = 44.0;
    /// The initial's size as a share of its disc.
    pub const INITIAL_FRAC: f32 = 0.5;
    /// The greeting drops its state word below this inner width.
    pub const GREETING_WORD_W: f32 = 220.0;
    /// A pill's horizontal padding as a share of its radius.
    pub const PILL_PAD_X: f32 = 0.7;
    /// The gap a tile leaves on each side (`Rect::to_px`).
    pub const TILE_GAP: u32 = 8;
    /// A pane's corner radius and a pill's ask.
    pub const PANE_RADIUS: f32 = 22.0;
    pub const PILL_RADIUS: f32 = 999.0;
    /// The words, at rest (the lock is up, nothing typed).
    /// The lock's button beside the field.
    pub const UNLOCK: &str = "Unlock";
    pub const MARKER: &str = "\u{203a}";
    pub const NOTE_READY: &str = "Enter your password to unlock";
    pub const WORD_LOCKED: &str = "locked";
    pub const WORD_UNLOCKED: &str = "unlocked";
    pub const HINTS: [(&str, &str); 2] = [("enter", "unlock"), ("esc", "clear")];
    /// The headings the lock gives its Glances (set small-caps by the view).
    pub const HEAD_BATTERY: &str = "battery";
    pub const HEAD_LINK: &str = "link";
    /// The account the builder's preview shows (display only).
    pub const SAMPLE_USER: &str = "robin";
}

/// A lock tile's padding and inner box. A pill's content keeps clear of its
/// curve: the horizontal pad is most of the radius, never less than the pane's
/// own, never more than a quarter of the width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockFrame {
    pub pad: f32,
    pub pad_x: f32,
    pub inner: Inner,
}

pub fn lock_frame(w: f32, h: f32, pill: bool) -> LockFrame {
    let pad = crate::tile::pad_for(w, h);
    let pill_r = if pill { w.min(h) / 2.0 } else { 0.0 };
    let pad_x = pad.max(pill_r * lock::PILL_PAD_X).min(w / 4.0);
    LockFrame {
        pad,
        pad_x,
        inner: Inner {
            w: (w - 2.0 * pad_x).max(0.0),
            h: (h - 2.0 * pad).max(0.0),
        },
    }
}

/// The greeting pill's parts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockGreeting {
    pub disc: f32,
    pub initial_px: f32,
    pub name_px: f32,
    /// The state word at the right, only when the pill is wide enough.
    pub word: bool,
}

pub fn lock_greeting(inner: Inner) -> LockGreeting {
    let disc = fit_in(inner.h * lock::AVATAR_FRAC, lock::AVATAR_MIN, lock::AVATAR_MAX).min(inner.h.max(0.0));
    LockGreeting {
        disc,
        initial_px: disc * lock::INITIAL_FRAC,
        name_px: ec_style_tokens::size::PROMPT,
        word: inner.w >= lock::GREETING_WORD_W,
    }
}

/// The password pane: the field, its note and, where there is room, the hints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockPassword {
    pub field_h: f32,
    pub hints: bool,
    /// The Unlock pill trims its padding to stay inside a short field.
    pub tight: bool,
}

pub fn lock_password(inner: Inner) -> LockPassword {
    let note_room = lock::NOTE_H + lock::NOTE_GAP;
    let hint_room = lock::HINT_H + lock::NOTE_GAP;
    let hints = inner.h >= lock::FIELD_MIN + note_room + hint_room;
    let avail = inner.h - note_room - if hints { hint_room } else { 0.0 };
    let field_h = fit_in(avail, lock::FIELD_MIN, lock::FIELD_MAX);
    LockPassword {
        field_h,
        hints,
        tight: field_h < lock::FIELD_ROOMY,
    }
}

// Every clamp takes its bounds in order; a reversed pair would panic.
const _: () = {
    assert!(GREETING_MIN < GREETING_MAX);
    assert!(FIELD_MIN < FIELD_MAX);
    assert!(ROW_MIN < ROW_MAX);
    assert!(SUBMIT_MIN < SUBMIT_MAX);
    assert!(AVATAR_MIN < AVATAR_MAX);
    assert!(lock::FIELD_MIN < lock::FIELD_MAX);
    assert!(lock::AVATAR_MIN < lock::AVATAR_MAX);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn boxes() -> Vec<Inner> {
        let mut v = Vec::new();
        for w in [0.0, 10.0, 80.0, 200.0, 301.0, 640.0, 1900.0] {
            for h in [0.0, 10.0, 40.0, 90.0, 170.0, 400.0, 1000.0] {
                v.push(Inner { w, h });
            }
        }
        v.push(Inner {
            w: f32::NAN,
            h: f32::NAN,
        });
        v
    }

    #[test]
    fn no_plan_exceeds_its_box() {
        for b in boxes() {
            if b.w.is_nan() {
                continue;
            }
            assert!(greeting(b) <= (b.h * GREETING_H).max(0.0) + 0.001);
            let p = password(b);
            assert!(p.field_h <= (b.h - NOTE_H - NOTE_GAP).max(0.0) + 0.001);
            assert!(p.submit_w <= b.w * SUBMIT_W_FRAC + 0.001);
            let w = power(b);
            assert!(w.pill_h <= b.h + 0.001);
            for n in [0, 1, 3, 9, 40] {
                let u = users(b, n);
                assert!(u.shown + u.more == n.max(u.shown));
                assert!(u.row_h <= ROW_MAX && u.avatar <= u.row_h + 0.001);
                if b.h >= HEAD_H + LINE_GAP + ROW_MIN && n > 0 {
                    assert!(u.used_h() <= b.h + 0.001, "{b:?} n={n} {u:?}");
                }
            }
        }
    }

    #[test]
    fn nan_never_panics() {
        let b = Inner {
            w: f32::NAN,
            h: f32::NAN,
        };
        let _ = (greeting(b), password(b), power(b), users(b, 3));
    }

    #[test]
    fn the_list_trades_a_row_for_the_more_line() {
        // Room for two rows, three accounts: one row and "+2 more".
        let h = HEAD_H + LINE_GAP + 2.0 * ROW_MIN + ROW_GAP;
        let u = users(Inner { w: 400.0, h }, 3);
        assert_eq!((u.shown, u.more), (1, 2));
        let u = users(Inner { w: 400.0, h: 1000.0 }, 3);
        assert_eq!((u.shown, u.more), (3, 0));
        assert_eq!(users(Inner { w: 400.0, h: 1000.0 }, 0).shown, 0);
    }

    #[test]
    fn short_power_labels_below_the_threshold() {
        assert!(
            !power(Inner {
                w: POWER_FULL_W - 1.0,
                h: 40.0
            })
            .full
        );
        assert!(
            power(Inner {
                w: POWER_FULL_W,
                h: 40.0
            })
            .full
        );
    }

    #[test]
    fn lock_plans_stay_inside_their_box() {
        for b in boxes() {
            if b.w.is_nan() {
                continue;
            }
            let g = lock_greeting(b);
            assert!(g.disc <= b.h.max(0.0) + 0.001);
            let p = lock_password(b);
            assert!(p.field_h <= lock::FIELD_MAX);
            let f = lock_frame(b.w, b.h, true);
            assert!(f.inner.w <= b.w + 0.001 && f.pad_x <= b.w / 4.0 + 0.001);
        }
        let _ = (
            lock_greeting(Inner {
                w: f32::NAN,
                h: f32::NAN,
            }),
            lock_password(Inner {
                w: f32::NAN,
                h: f32::NAN,
            }),
        );
    }

    #[test]
    fn the_lock_shows_hints_only_with_room_for_them() {
        let need = lock::FIELD_MIN + lock::NOTE_H + 2.0 * lock::NOTE_GAP + lock::HINT_H;
        assert!(lock_password(Inner { w: 400.0, h: need }).hints);
        assert!(
            !lock_password(Inner {
                w: 400.0,
                h: need - 1.0
            })
            .hints
        );
        assert!(
            !lock_greeting(Inner {
                w: lock::GREETING_WORD_W - 1.0,
                h: 40.0
            })
            .word
        );
    }

    #[test]
    fn the_radius_is_clamped_to_half_the_short_side() {
        assert_eq!(radius(PILL_RADIUS, 40.0), 20.0);
        assert_eq!(radius(CARD_RADIUS, 500.0), CARD_RADIUS);
    }
}
