// SPDX-License-Identifier: AGPL-3.0-only
//! The EclipseOS welcome screen: step 0 of first-run setup (D-07).
//!
//! A moon crosses a sun, the eclipse shrinks into the O of the logo, ten
//! greetings fly past, and a spacebar keycap asks to be pressed. Everything is
//! one iced canvas driven by frame ticks; what is on screen is a pure function
//! of elapsed time ([`timeline`]).
//!
//! It is a library so `eclipse-setup` can embed it as its first step:
//!
//! ```ignore
//! let mut welcome = eclipse_welcome::Welcome::new(env!("CARGO_PKG_VERSION"))
//!     .reduced_motion(eclipse_welcome::reduced_motion_from_env());
//! // update:       welcome.update(msg).map(Msg::Welcome)
//! // view:         welcome.view().map(Msg::Welcome)
//! // subscription: welcome.subscription().map(Msg::Welcome)
//! // and advance to step 1 when you see `eclipse_welcome::Message::Begin`.
//! ```
//!
//! Not TCB, no network, writes nothing.

mod draw;
pub mod fonts;
pub mod palette;
pub mod timeline;
mod welcome;

pub use welcome::{reduced_motion_from_env, Message, Welcome, REDUCED_MOTION_ENV};
