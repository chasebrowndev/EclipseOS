// SPDX-License-Identifier: AGPL-3.0-only
//! Window classification values a `window-rule` can name (COMP-07 §2). Plain
//! data; the clamping logic lives in `ec-abyss`'s `xwayland::security`.

/// How far the compositor trusts the application behind a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppTrust {
    Standard,
    Trusted,
}

/// Whether a second (agent) seat can address the window concurrently
/// (COMP-04 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Multi` exists for Wayland windows; X11 never gets it
pub enum SeatCompat {
    /// Multiple seats may be routed to the window at once.
    Multi,
    /// One seat at a time, taken under an exclusive focus lock.
    Lock,
}
