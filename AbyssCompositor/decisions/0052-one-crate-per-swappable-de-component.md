# 0052 — One crate and one package per swappable DE component
Status: accepted
Date: 2026-09-21
Deciders: chase (owner), Claude (advisory)

## Context
`crates/eclipse-bar` built four binaries: the taskbar, plus `eclipse-toasts`,
`eclipse-center` and `eclipse-launcher` as extra `[[bin]]` targets, and all
four shipped in the single `eclipseos-desktop` package. None of the three
extras imports anything from the bar; they only share `eclipse-ui` and
`eclipse-services`, which are separate crates already. The bundling was an
accident of layout, but a real cost to users: someone who runs Quickshell or
Waybar as their bar and wants our notification toasts had to install our bar
to get them.

EclipseOS is a WM/DE hybrid (PROPOSEDFEATURES). The DE layer should be a set of
parts a user can take or leave, and each part should be replaceable by a
third-party alternative.

## Options
1. **Keep the multi-bin crate, split only the packages.** One crate still
   compiles all four, and the package boundary drifts from the code boundary.
2. **One crate per component, one package per crate.**
3. **One crate per component, one package for all of them.** Code is modular
   but installs are not, which was the whole complaint.

## Decision
Option 2. The taskbar crate is renamed `hyperion` (internal name; the UI and
docs addressed to users say "taskbar"). `eclipse-toasts`, `eclipse-center` and
`eclipse-launcher` become their own crates. Each ships as its own package
(`eclipseos-hyperion`, `eclipseos-toasts`, `eclipseos-center`,
`eclipseos-launcher`), and `eclipseos-meta` depends on all of them, so a
default install is unchanged.

Rules going forward:
- A swappable component never depends on another one. Shared code goes into
  `eclipse-ui` (visual) or `eclipse-services` (data and D-Bus).
- Only `eclipseos-hyperion` depends on `eclipseos-abyss`. The others are plain
  layer-shell clients and must keep working under any compositor that speaks
  wlr-layer-shell.
- Cross-component launches use the binary name on `PATH` (the taskbar spawns
  `eclipse-launcher` and `eclipse-center` that way), so a replacement binary of
  the same name slots in.

## Consequences
- `eclipseos-toasts` installs and runs with no taskbar and no abyss.
- `eclipse-bar.service` becomes `hyperion.service`, and `ECLIPSE_BAR_OUTPUT`
  becomes `HYPERION_OUTPUT`. There is no compatibility alias: EclipseOS has no
  installed base to keep.
- Four `cargo build` targets instead of one crate. The build graph is the same
  because they already shared their dependencies.
- D-01 §1.1's package table is updated to match.
