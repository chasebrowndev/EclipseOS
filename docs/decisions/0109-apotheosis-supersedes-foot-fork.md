# 0109 — Apotheosis: a first-party terminal supersedes the foot fork
Status: accepted
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0034 chose a foot fork (`cataclysm`) for v1 and deferred a from-scratch emulator. The fork never started (STATUS milestone 23). A fork carries C into a Rust tree, cannot draw the glass interface, and inherits foot's deliberate limits (no tabs, no splits, no kitty graphics, parse-once config). The owner has un-deferred the from-scratch option as **Apotheosis**.

## Options
1. Keep ADR 0034: fork foot. Fastest to a working terminal; permanently a C codebase we patch, with none of the DE-level features.
2. Adopt an existing Rust terminal (alacritty, wezterm, rio) and patch it. Rust, but someone else's architecture, toolkit and config language; integration stays skin-deep.
3. Write our own, in Rust, with its own Wayland client and wgpu renderer, to the T-NN spec volume.

## Decision
Option 3. Binary `apotheosis`, CLI `apoth`, crates `apoth-*`, workspace `apotheosis/`, spec family T-NN in `docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`. Wayland-only, any compositor; EclipseOS integration lights up on Abyss. On Abyss glass uses `ext_background_effect_v1` only; off Abyss the T-09 §5 ladder applies (ext → KDE blur → tint). No private Eclipse glass protocol and no allow-list entry. ADR 0034 is superseded; the name `cataclysm` is retired; `ec-cataclysm-pub` is renamed by ADR 0114.

## Consequences
A large build (T-00 §7, forty milestones) instead of a patch set. foot remains the default terminal until milestone M31 and an optional dependency for one release after. The spec-trail job accepts `T-NN`. D-01, D-05 and D-07 are amended at the default switch (T-24 §5), not before.

## Revisit when
M18 (daily-drivable alpha) slips past two consecutive owner checkpoints, or the T-23 budgets prove unreachable with the chosen architecture.
