// SPDX-License-Identifier: AGPL-3.0-only

//! Pure portal logic: no D-Bus, no I/O. See docs/design/D-08-portal.md.
//!
//! The daemon converts between D-Bus variants and [`val::Val`] at its edge, so
//! everything here is unit-testable without a bus.

pub mod account;
pub mod email;
pub mod filechooser;
pub mod frame;
pub mod identity;
pub mod notification;
pub mod openuri;
pub mod options;
pub mod request;
pub mod screencast;
pub mod settings;
pub mod val;
