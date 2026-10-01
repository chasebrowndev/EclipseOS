# 0068 — The wallpaper is a core daemon on the Background layer
Status: accepted
Date: 2026-09-27
Deciders: chase (owner), Claude (advisory)

## Context
D-05 §7 lists "wallpaper setter" among the things the DE does not do. Today the
desktop behind every window is whatever abyss clears to, and a user who wants
an image has to bring a third-party layer-shell client and start it by hand.

Forces:
- abyss already has `Layer::Background` and draws layer-shell surfaces on it;
  nothing new is needed in the compositor for a client to paint there.
- D-01 §6.3: no default wallpaper, theme or font ships in `/etc/eclipse`.
  Appearance belongs to Settings and D-05, not to the image.
- Image decoding is a large, historically bug-prone input surface. The
  compositor is TCB.
- Every desktop has a background, even if it is one flat colour.

## Options
1. **A core daemon.** `eclipse-wallpaper`, an out-of-process wlr-layer-shell
   client on `Layer::Background`, one surface per output, drawing an image
   (`fill`, `fit`, `center`) or a solid colour. Configured by a `wallpaper`
   node in `abyss.kdl`; ships as its own package, a dependency of
   `eclipseos-meta`. Pros: no TCB growth, one crate per component (ADR 0052),
   swappable. Cons: one more process and unit.
2. **An add-on under ADR 0066.** Rejected: add-ons are optional, and a
   background is not. With the add-on absent the user still sees a wallpaper,
   the bare `color::BASE` fill, so the "off" state is the same feature with
   fewer options, not its absence.
3. **Drawn inside abyss.** Rejected: it grows the TCB, and image decoding does
   not belong in the compositor's process or loop.

## Decision
We take **1**. `eclipse-wallpaper` is a core component, crate
`shell/crates/ec-wallpaper/`, package `eclipseos-wallpaper`, started by
`eclipse-wallpaper.service` (`WantedBy=abyss-session.target`) like the other DE
units. It reads the top-level `wallpaper` node through `get_config`: `path`
(optional), `mode` (`fill` | `fit` | `center`, default `fill`), `color`
(default `color::BASE`, `#0b0906`), and optional `output "<name>" { path …;
mode …; }` overrides, KDL-only in v1. An unset or unreadable `path` draws the
solid colour. It re-reads on `EventKind::Config` and reconciles its surfaces on
`EventKind::Output`, the way hyperion does. Settings shows the global keys as
schema-driven rows in the Desktop pane. D-01 §6.3 still holds: no image
ships in `/etc/eclipse`, and the default wallpaper is solid `color::BASE`.

## Consequences
- D-05 §7 drops "wallpaper setter"; the other exclusions stand.
- The image is decoded as the user, in a non-TCB process; a hostile file can
  crash the daemon, not the compositor. `Restart=on-failure` brings back the
  colour fill.
- New schema keys `wallpaper.path`, `wallpaper.mode`, `wallpaper.color`, and a
  per-output child the Settings coverage test must list as an exception until a
  picker exists (`ci/gui-coverage-exceptions.txt`).
- Settings needs a path control for `wallpaper.path`; today `string` maps to a
  text field (see the splash entry in `docs/PROPOSEDFEATURES.md`).
- A third-party wallpaper client still works: stop the unit, run your own.
- We owe `docs/CONFIG.md` (generated from the schema), D-05 §2/§5 rows, and the
  unit and PKGBUILD split.

## Revisit when
Animated or slideshow wallpapers are wanted: frame timing, a directory watch
and a timer change what the daemon costs while idle.
