<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# steam-glass

The Eclipse glass theme for Steam's desktop UI. An add-on (ADR 0066) with no
host hook: a manifest, an injector process, and two assets.

| Piece | Where |
|---|---|
| Manifest | `packaging/addons/steam-glass.kdl` |
| Injector | `shell/crates/ec-steam-glass` (`ec-steam-glass`, user unit `ec-steam-glass.service`) |
| Base layer | `steam-glass.css`: stable variables, scrollbars, no opaque ground |
| Recolouring | `steam-glass.js`: rewrites Steam's hard-coded surfaces by colour |

## How it works

Steam's client UI is a Chromium Embedded Framework app. Steam opens a
DevTools port on `127.0.0.1:8080` at start-up when
`~/.steam/steam/.cef-enable-remote-debugging` exists (the injector creates it;
it takes effect the next time Steam starts). The injector holds one connection
per Steam page, registers the bundle for every new document, and runs it in the
current one. Editing the assets under `--assets DIR` re-applies them live.

Only Steam's own pages are touched: `about:blank?createflags=` windows and
`steamloopback.host`. The store and community pages are other people's sites.

Steam's classes are hashed and change every release, so `steam-glass.js` keys
on **colour values** (`#0e141b`, `#171d25`, `#23262e`, `#1a9fff`, ...): it walks
each stylesheet and, for every rule painting one, appends a same-selector rule
with the glass equivalent. Surface greys become smoked-glass tints and
white-alpha steps from `docs/STYLE.md`; Steam's blue accent becomes gold,
used for state only.

## The window needs blur from abyss

The theme leaves pixels transparent; the compositor draws the glass behind them.
Steam is an XWayland client, so a window rule is needed if your config turns
blur off for X11 windows:

```kdl
windowrule "blur glass" { app-id "^steam$"; title "^Steam$"; }
```

Match the main window by title: Steam's popups (Friends List, Settings) are
separate windows and float.

## Development

```
cargo build -p ec-steam-glass          # in shell/
target/debug/ec-steam-glass --assets packaging/addons/steam-glass
```

Run against a Steam in a hidden headless nested session, never the live one.
