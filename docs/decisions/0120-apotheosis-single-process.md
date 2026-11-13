# 0120 — One process per instance by default; the daemon is opt-in
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
Terminals split three ways: process per window (alacritty default), one process with many windows (kitty, ghostty, foot server), and a persistent mux daemon with GUI clients (wezterm, zellij, tmux).

## Options
1. Process per window. Crash isolation; slow window open, duplicated atlases and fonts.
2. Daemon from day one. Sessions survive the GUI; doubles the surface before first light.
3. One process, many windows, with a second launch handing off over the control socket; keep the mux model free of Wayland and GPU so a daemon is a later relocation.

## Decision
Option 3 (T-21 §1). `--new-instance` opts out. The daemon (`apotheosis --server`, T-21 §3) is R2 and opt-in until it has a release of soak.

## Consequences
`START-WARM` is cheap. A crash takes every window of the instance; fuzzing and the soak gates carry that risk until the daemon exists.

## Revisit when
Crash reports show multi-window loss in practice, or the daemon completes its soak (owner decides the default at M35 exit).
