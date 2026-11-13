# 0121 — Scrollback persistence and disk spill are off by default, encrypted, and sensitivity-aware
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
Unlimited scrollback and session restore invite writing terminal contents to disk. Terminal contents routinely include secrets; root invariants forbid storing `password`-role values.

## Options
1. Never write contents to disk. Simple; bounds history by RAM (mitigated by compressed cold pages).
2. Write freely under `0600`. Leaves secrets at rest.
3. Off by default; when enabled, encrypt with a random key generated at process start, held only in that process's memory, never written to any file, keyring, socket or environment (cipher `chacha20poly1305 =0.10`, RustCrypto, Apache-2.0 OR MIT), and exclude any pane that has ever been in the credential state.

## Decision
Option 3 (T-18 §9, T-21 §5). Session files hold layout, cwd, argv and profile only. Defaults: `scrollback.limit-bytes` 50 MiB per pane in memory with lz4-compressed cold pages; no spill.

## Consequences
Under the daemon, history survives GUI/compositor restarts and detach/attach but not a daemon restart; stale spill files are deleted unread on start. History does not survive a reboot even when persistence is on (process-lifetime key) — deliberate. The matrix row for persisting evicted scrollback stays `no`.

## Revisit when
Users ask for cross-boot history, which would need a keyring-held key and its own ADR.
