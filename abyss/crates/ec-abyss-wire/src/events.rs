// SPDX-License-Identifier: AGPL-3.0-only
//! Subscribable event kinds.

/// Event kinds a client may ask for (COMP-13 §2.1). An unknown kind is
/// rejected rather than silently accepted: a bar that thinks it is subscribed
/// and never hears anything is the worst outcome.
pub const EVENTS: &[&str] = &[
    "workspace",
    "window",
    "focus",
    "output",
    "agent-activity",
    "config-error",
    "config",
    "keybind",
    "launcher",
    // The bar's power drawer, same payload as `launcher`.
    "power",
    // COMP-10 §2: whether a personal secret is set. Never the phrase.
    "phrase",
    // A-08 §7: how many consent prompts are parked. A count, never content.
    "decisions_pending",
    // ADR 0084: the effective scheme or the active theme's seeds changed.
    "theme",
    // ADR 0106: a human capture (screen share, recording) started or stopped:
    // `{"active": bool}`. No consumer, no counts.
    "capture",
    // ADR 0102: a contained panic turned a feature off for the session:
    // `{"feature": "<name>"}`, plus `"method"` when one query method went off. Replayed on `subscribe` while it stands.
    "feature-disabled",
    // ADR 0099: `{locked, pending, locker_alive}`, on every change.
    "lock",
];
