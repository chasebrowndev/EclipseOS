// SPDX-License-Identifier: AGPL-3.0-only
//! Desktop services for the Eclipse userland (ADR 0038).
//!
//! Everything the bar and the control center need from the wider session that
//! the compositor deliberately does not provide: notifications first, then the
//! tray, audio, network, Bluetooth and session control.
//!
//! Nothing here is in the TCB. These are ordinary userland services running as
//! the human, outside `abyss`; they hold no capability and enforce no policy.
//! A compromised service can annoy the human, not escalate.

pub mod notifications;
pub mod status;
