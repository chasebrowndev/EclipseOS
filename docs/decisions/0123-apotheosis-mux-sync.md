# 0123 — Mux sync by snapshot plus byte-stream tee
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
When a GUI attaches to a mux daemon it needs the daemon's terminal state. wezterm sends cell-level deltas (a protocol at codec version 46); tmux re-renders escape sequences for the client; zellij sends rendered frames.

## Options
1. Cell-diff protocol. Bandwidth-efficient; a second source of truth for rendering that must track every grid feature.
2. Re-render to escape sequences. Lossy for anything the outer encoding cannot express.
3. Versioned binary snapshot on attach, then tee the PTY byte stream with sequence numbers; the view runs the same deterministic `apoth-term`.

## Decision
Option 3 (T-21 §3). Correctness follows from `feed` being deterministic (T-25 §1). A slow view is resynchronised from a fresh snapshot rather than blocking the PTY.

## Consequences
Each view spends CPU parsing; version skew between server and view must be refused across major versions. Images travel by memfd locally and inline remotely.

## Revisit when
Remote links prove too slow for raw byte streams, which would motivate a compressed or diff transport.
