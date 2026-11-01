# 0079 — Glass for Firefox: a theme add-on with compositor-side translucency
Status: accepted; strip rendering and the new-tab page amended by 0080
Date: 2026-10-30
Deciders: chase (owner), Claude (advisory)

## Context
The owner wants Firefox to match the DE: dark, gold accent, liquid glass behind
the tab strip and toolbar, everything configurable, shipped as an add-on
(ADR 0066) like hyperion and anim-pack.

A spike on Firefox 158 (Wayland, headless abyss) showed Firefox never submits a
toplevel with alpha. Theme colours with alpha, `browser.tabs.allow_transparent_browser`,
`widget.transparent-windows`, the opaque-region and WebRender prefs, and
`userChrome.css` all end up composited over an opaque base. Abyss's whole-window
`opacity` rule would make page text translucent. A WebExtension also cannot
change chrome geometry.

Release Firefox loads only Mozilla-signed extensions, including system-installed
ones.

## Options
1. **Opaque glass look.** Theme fakes glass with a wallpaper copy. No real
   translucency; follows wallpaper only through a helper.
2. **Patched Firefox build.** True per-pixel alpha, but we rebuild Firefox every
   release, carry a widget/gtk patch and must rebrand.
3. **Compositor strip translucency.** abyss gains a `translucent-region`
   window-rule action: the top N logical px of a window draw at an alpha with the
   window's blur mode behind them, overriding the client's opaque region and an
   empty `ext_background_effect_v1` area for that strip. The extension paints the
   strip in a tint tuned for it.

## Decision
We take **3**.

- **abyss** gets `translucent-region top=<px> opacity=<0..1>` (COMP-05 §4
  action, COMP-02 §9 alpha and blur gate) and a `floating #true|#false` matcher,
  because Firefox's chrome is a different height floating and tiled. Both are
  generic: any opaque client can use them, and neither names Firefox. The strip
  is drawn identically in the live and the capture (`still.rs`) paths.
  The strip is the one opt-in exception to "content is never faded to obtain
  glass" (C-21, amending C-14); rule timing is written down as built (C-22).
- **The add-on** `eclipseos-firefox-glass` ships:
  - a manifest `/usr/share/eclipse/addons/firefox-glass.kdl` turning on the
    `client-strips` hook;
  - the signed XPI `/usr/share/eclipse/firefox/glass.xpi`;
  - `/etc/firefox/policies/policies.json`, which force-installs the XPI
    (`normal_installed`) from that `file://` URL. Nothing is fetched at runtime;
  - `ec-glass-host`, a native-messaging helper, and its host manifest.
- **No setup step; the strip follows Firefox live.** The extension sends each
  window's outer size and active-tab height (`tabs.Tab.height`, no URLs or
  titles) to `ec-glass-host`, which calls `set_client_strips` on the control
  socket. abyss sets `top = window geometry height - page height`, so tiling,
  floating, zoom and toolbar changes are followed without knowing Firefox's CSD
  shadow. Strips belong to the connection: when Firefox exits the helper exits
  and abyss clears them.
- **Authority.** `set_client_strips` is refused unless `client-strips` is on and
  the caller is the owner uid, and it reaches only toplevels whose client pid is
  the caller's parent pid (the Firefox that spawned the helper). It is narrower
  than `set_config_value`. A user `translucent-region` rule in `abyss.kdl` wins
  over a client strip. The package writes no abyss config, so `/etc/eclipse`
  stays theme-free (D-01 §6.3).
- **Signing.** AMO unlisted channel (`web-ext sign --channel unlisted`). The owner
  holds the AMO API keys; the signed XPI is committed, and CI checks that its
  contents equal the source tree, so the signed artifact cannot drift from the
  reviewed source.
- **Name.** "Glass for Firefox": Mozilla's naming rule allows "X for Firefox";
  no Mozilla logo, no implied endorsement.

## Consequences
- A system administrator who already has `/etc/firefox/policies/policies.json`
  conflicts with the package. Pacman refuses the file conflict; v1 accepts that.
- The extension keeps no telemetry (`data_collection_permissions: none`) and needs
  Firefox ≥ 142.
- A bottom findbar or docked devtools shortens the page area, so the strip
  grows by that height while it is open.
- `gate.rs` gains one entry; that is authz enforcement and gets owner review.

## Revisit when
- Firefox ships an alpha toplevel on Wayland: drop the strip rule for it.
- A third-party app add-on needs more than a strip (side panels, per-region
  shapes): that is `ext_background_effect_v1` territory, not more rule keys.
