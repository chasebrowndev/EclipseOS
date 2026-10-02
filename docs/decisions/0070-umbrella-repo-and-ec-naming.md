# 0070 — One umbrella repo, a workspace per product, `ec-` names
Status: accepted
Date: 2026-10-01
Deciders: chase (owner), Claude (advisory)

## Context
Everything lived under `AbyssCompositor/` in one Cargo workspace, including the
shell apps, installer and Fog. Crate names mixed `hyperion`, `policyd`,
`eclipse-*` and `fog-*`. `ec-settings` and `ec-ctl` depended on the whole
compositor library for the config schema.

## Options
1. Keep one workspace, rename only. Cheap, leaves the coupling.
2. One repo, one Cargo workspace per product, shared client crates by path.
3. One repo per product. Cross-repo path deps break; the gate loses atomicity.

## Decision
Option 2. Layout: `abyss/` (compositor, `ec-ipc`, `ec-ctl`, policy crates),
`shell/` (bar, launcher, settings, center, toasts, wallpaper, viewers, `ec-ui`,
`ec-services`), `installer/`, `oracle-eyes/`, `fog/`, plus `packaging/`,
`assets/`, `docs/`. All workspaces share `<repo>/target` via `.cargo/config.toml`.
Every crate, binary, unit and `.desktop` file is named `ec-<thing>`
(`ec-hyperion-bar`, `ec-settings`, `ec-abyss`, `ec-fogd`).

Kept stable: config files (`abyss.kdl`, `policy.kdl`, `oracle-eyes.kdl`),
sockets, env vars, app-ids (`os.eclipse.fog`), polkit/dbus ids, session identity
(`abyss.desktop`, `XDG_CURRENT_DESKTOP=abyss`), the pacman package `eclipseos`,
and the signed-grant issuer `"policyd"`.

The config loader accepts legacy component ids (`bar "hyperion"`,
`eclipse-launcher`) as aliases for one release.

## Consequences
Amends 0052 (one crate per swappable component): the rule stands, the names and
directories change. CI runs per workspace; the TCB path regex follows the move.
Config extraction and compositor crate splitting follow as separate stages.

## Revisit when
A product needs its own release cadence or repo.
