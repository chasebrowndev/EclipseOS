# 0124 — `TERM=xterm-apotheosis`
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
A new terminal must choose a `TERM`. Claiming `xterm-256color` forfeits our capabilities in terminfo; a novel name breaks software that keys on the `xterm-` prefix and every remote host without our entry.

## Options
1. `xterm-256color`.
2. `apotheosis`.
3. `xterm-apotheosis` (+ `-direct`), with a probe that falls back to `xterm-256color` when the entry is not installed.

## Decision
Option 3 (T-19). The entry is generated from one Rust table that also answers XTGETTCAP; a pure-Rust writer emits the compiled form, so `tic` is not a build dependency.

DA2 is `CSI > 1 ; Pv ; 0 c`, Pv = major×10000 + minor×100 + patch (foot's encoding); vim's DA2 heuristics are covered by the T-19 §5 interop fixture.

## Consequences
The entry ships in our package; remote hosts get it through `apoth ssh` or `apoth terminfo`. Submitting the entry to ncurses upstream is owed after M31.

## Revisit when
ncurses accepts the entry under a different name.
