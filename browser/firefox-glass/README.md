<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Glass for Firefox

An EclipseOS add-on (ADR 0066) that makes Firefox match the desktop: warm near-black, gold accent, liquid glass. Firefox on Wayland always submits an opaque toplevel and flattens theme alpha, so the translucency is done by abyss, keyed on colour: the extension paints every glass surface in a reserved key colour, and abyss makes exactly the pixels of those colours translucent (with blur behind). Text, icons and page content are other colours and stay opaque.

There is no new-tab override: Firefox's own Home page (recent pages, shortcuts, settings) is back. Home is themed through `ntp_background` and `ntp_card_background`, which are key colours too.

## Keyed glass

Four keys, derived from the tint in `shared/glass.js` (`keyColors`): base glass (frame, tab strip, toolbar, Home ground), URL field and selected tab, popups and menus, sidebar and Home cards. Each key is the intended colour moved by exactly one unit in one channel, so it is invisible to the eye but not a colour a page would paint. Keys are pushed at least 2 units apart so abyss's +-1 rounding tolerance cannot confuse them. The popup key is flagged `everywhere` so abyss also keys it over the whole window on websites (the URL-bar results dropdown is drawn inside the window); the focused URL field and that dropdown are painted in it. Each key has its own opacity: Strength scaled by the matching region's slider. The default Strength gives about 35% on the base glass.

## What leaves the browser

Over native messaging (`os.eclipse.glass`, one long-lived port; reconnects with backoff): the key colours and opacities, and per window the size, the active tab's page-area height, and a `body` boolean that is true while the active tab is about:home, about:newtab, about:blank or about:privatebrowsing (abyss then keys the whole window). The `tabs` permission is needed only to read the URL for that test; the URL is never sent, only the boolean. No titles. Updates are sent on window, tab, URL and zoom events and a 1 s poll for resizes, and only when the payload changed. The options page shows whether the helper is connected.

Limit: the page area is what is measured, so a bottom findbar or docked devtools shortens it and the strip grows while they are open.

A WebExtension can only set theme colours, so it cannot change geometry, radius or density, draw behind the page, or blur anything. Load it with `npx web-ext run -s browser/firefox-glass` (throwaway profile) and check it with `npx web-ext lint -s browser/firefox-glass`.
