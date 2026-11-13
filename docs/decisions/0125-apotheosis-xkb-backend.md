# 0125 — Pure-Rust XKB backend with libxkbcommon as an optional feature and test oracle
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
Key handling needs XKB keymap compilation, state and Compose. `libxkbcommon` is the reference C library (allow-listed). `kbvm` is a pure-Rust implementation with a clean licence tree, MSRV 1.95, one maintainer, and nine releases since January 2025.

## Options
1. libxkbcommon only. Reference behaviour; one more linked C library on the input path.
2. kbvm only. No C; bus factor of one.
3. kbvm by default behind `trait KeyboardBackend`, with a `xkbcommon` cargo feature as an escape hatch and a dev-only differential oracle that links libxkbcommon through hand-written `extern` declarations.

## Decision
Option 3 (T-05). Compose tables are read from the system X11 locale data (an optional dependency).

## Consequences
The differential test (every key × modifier set across the shipped layouts) is part of the gate. If kbvm stalls, flipping the default feature is a one-line change. Known kbvm differences (starting with modifier-qualified Compose lines and consumed-modifier modes) live in a reviewed divergence file; Compose table equality excludes modifier-qualified lines, which are compared separately. The corpus runs from M7 (CI-N) and must pass, with divergences fixed or accepted, at M18 exit; otherwise the default flips to xkbcommon by amending this ADR (T-05 §8).

## Revisit when
kbvm goes unmaintained for six months, or the oracle finds a divergence upstream will not fix.
