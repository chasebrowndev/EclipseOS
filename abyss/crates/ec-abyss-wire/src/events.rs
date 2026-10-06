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
    // COMP-10 §2: whether a personal secret is set. Never the phrase.
    "phrase",
    // A-08 §7: how many consent prompts are parked. A count, never content.
    "decisions_pending",
];
