// SPDX-License-Identifier: AGPL-3.0-only
//! The notification stack: a layer surface of its own, top-right.
//!
//! It is a separate surface from the bar because the bar is one 34px row with
//! no room in it, and because the two have different lifetimes on screen: the
//! bar is always there, the stack exists only while something is waiting to be
//! read. The service behind it is `eclipse_services::notifications`, our own
//! `org.freedesktop.Notifications` implementation (ADR 0038).
//!
//! Like the bar, this is an ordinary Wayland client outside the TCB. It draws
//! what applications sent and reports what the human clicked; it holds no
//! capability and can grant none.

pub mod app;
pub mod view;

/// Width of the stack's surface, including its outer padding.
pub const WIDTH: u32 = 404;
