# 0084 — Themes: derived palettes, dark and light as built-in presets
Status: accepted
Date: 2026-11-02
Deciders: chase (owner), Claude (advisory)

## Context
Dark is not a theme today. It is built into the code. `ec-ui` `tokens::color`
is about 70 `pub const`s, mostly white alphas over warm-black. `theme::theme()`
returns one fixed iced `Theme`. The compositor bakes dark in as well: the frost
tint (`FROST_TINT`), the white rim, the shadow colour, the HUD palette
(`ec-abyss-render/src/palette.rs`) and the trusted-UI prompt colours. Client
apps get no scheme hint (no `org.freedesktop.appearance color-scheme`).

The owner wants a light mode, and user-configurable themes after it. Dark
stays the default. 70 hand-picked tokens per theme is too many to configure,
and too many to keep consistent with STYLE.md.

## Options
1. **Hand-written palettes.** A full token list for dark and another for
   light. Simple, but a user theme would mean 70 values with no guarantee
   they obey STYLE.md.
2. **Derived palettes from a few seed colours.** One function maps seeds to
   every token. Dark and light are two built-in seed sets. A user theme
   overrides seeds. The pattern already exists: `annotations { … }` owner
   colours and the palette derived from them (ADR 0071).
3. **Per-pane theme settings.** The panes and the compositor drift apart,
   and client apps still get no hint.

## Decision
Option 2, with one config source in `abyss.kdl` that the compositor
distributes.

**Seeds.** `base`, `ink`, `accent`, `frost-tint`, `rim`, plus optional
`accent-text` (otherwise derived). `derive(seeds) -> Palette` produces every
token. The built-in `"dark"` seeds must make `derive` reproduce today's
tokens exactly; the built-in `"light"` seeds produce STYLE.md "Light".

**Config.**
```
general {
    color-scheme "auto"        // "dark" (default) | "light" | "auto"
    theme-dark   "dark"        // a theme name used when the scheme is dark
    theme-light  "light"
    light-from   "07:00"       // auto schedule, local time
    dark-from    "19:00"
}
theme "ember" {                // user theme; unset seeds inherit from
    inherit "dark"             // the named built-in
    accent "#e8873a"
}
```
`"auto"` switches on a fixed local-time schedule. It does not use
sunrise/sunset, because that needs location and location is sensitive data.
A theme's brightness (dark or light) is computed from its `base` luminance.
It sets the effective scheme published to clients and picks the trusted-UI
palette.

**Validation (fail-closed, at `config validate`).** A theme is rejected,
and the last good config kept, if text or accent text on its base is
below 4.5:1, if secondary text is below 3:1, or if a seed fails to parse.
There are no gradient seeds and no second accent: STYLE.md's accent
discipline holds for every theme.

**Distribution.** The compositor holds the effective palette and applies it
to frost tint, rim, shadow and the HUD palette. It sends the seeds over the
control socket, so shell panes run the same `derive` (shared crate, no
smithay) and re-render. `ec-services` publishes `org.freedesktop.appearance
color-scheme` (1 = prefer dark, 2 = prefer light) and `accent-color`.

**Trusted UI (amends COMP-10 §5).** Prompts follow the *scheme*, not the theme. They use the
built-in dark or light palette, compile-time constants in the TCB, matching
the active theme's brightness. User config never reaches the TCB as a
colour.

**Details as built.** Seeds are opaque `#rrggbb`; a theme may inherit only a
built-in or an earlier theme, so cycles cannot form. Theme blocks are KDL-only,
not settable over the socket. An explicit `blur { frost { tint } }`,
`blur { glass { rim } }` or `annotations { }` key still wins over the theme.
Light glass whitens once (amended): the compositor's glass only veils the
backdrop lightly, as dark's smoke does, and the window tint carries the
white. Whitening in both stacked into a near-opaque window (STYLE.md Light).
`ec-portal` (`ec-services`) is the bus-activated portal Settings backend,
selected by `abyss.portal` and `abyss-portals.conf` under
`XDG_CURRENT_DESKTOP=Abyss`. It serves dark until the compositor answers.

## Consequences
- COMP-10 §5 ("no theming") is amended in the same PR to allow exactly
  this, and nothing more: two compiled-in palettes chosen by the scheme.
- **TCB change (line-by-line owner review):** trusted-UI prompt colours now
  depend on config, but only as a choice between two fixed palettes. Colour
  was never what proves a prompt is genuine: the prompt is still
  compositor-drawn and modal, with its dimmed backdrop and fixed placement
  (COMP-10). A client can read the scheme but cannot set it.
- `tokens::color` becomes a runtime `Palette` built by `derive`. That is a
  mechanical sweep of about 370 call sites. Dark output must be
  pixel-identical before and after (screenshot diff + a unit test that
  `derive(DARK)` equals the old constants).
- `derive` lives in a small state-free crate that both `ec-ui` and
  `ec-abyss-render` link, so the compositor and the panes cannot disagree.
- Every pane needs a light visual pass against STYLE.md "Light". A Settings
  theme picker (scheme, dark/light theme, accent) is later frontend work.
- `docs/CONFIG.md`, the schema and COMP-02 §9 (material colours are now
  theme-dependent) are updated in the PRs that land them.
- A switch is live: no pane restart.

## Revisit when
Per-app theme overrides are requested (a window rule). Or a location
service with its own consent path lands, at which point `auto` could follow
sunrise/sunset. Or users ask for more seeds than the five, which may mean
STYLE.md itself needs another variable.
