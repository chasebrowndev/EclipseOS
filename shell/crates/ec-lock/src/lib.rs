// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-lock`, the screen locker (ADR 0095).
//!
//! An `ext_session_lock_v1` client. The compositor holds the session shut; this
//! process only draws the lock surfaces and decides, through PAM, when to send
//! `unlock_and_destroy`. Dying is never a way in: the compositor keeps the
//! session locked behind its own fallback (ADR 0024).
//!
//! Layout, from the bottom up:
//! - [`secret`]: the password buffer. Never `Debug`, never `Clone`, wiped.
//! - [`pam`]: libpam over FFI, and [`pam::Granted`], the only proof of success.
//! - [`machine`]: the lock's state machine. Pure, no iced, no Wayland.
//! - [`auth`]: runs PAM on a worker thread, answers by channel.
//! - [`app`]: the iced model, its messages and `update`; the one place
//!   `Message::Unlock` is built.
//! - [`view`]: placeholder drawing, owned by eclipse-frontend.

pub mod app;
pub mod args;
pub mod auth;
pub mod clock;
pub mod instance;
pub mod machine;
pub mod pam;
pub mod secret;
pub mod service;
pub mod user;
pub mod view;

/// The PAM service this locker authenticates against: `/etc/pam.d/ec-lock`.
pub const PAM_SERVICE: &str = "ec-lock";
