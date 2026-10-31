<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Glass for Firefox

An EclipseOS add-on (ADR 0066) that makes Firefox match the desktop: warm near-black, gold accent, liquid glass. Firefox on Wayland always submits an opaque toplevel and flattens any theme alpha, so the translucency is done by abyss, not by the theme. abyss draws the glass over only the top strip of the window (tab strip and toolbar) and leaves the page opaque. This extension paints that strip in an opaque, dark tint, and gives popups, the sidebar and the new-tab page the same material by look only.

There is no setup. The extension reports each window's size and its active tab's page-area height over native messaging (`os.eclipse.glass`, one long-lived port; reconnects with backoff). abyss works out the strip as window height minus page height, so it follows tiling, floating, fullscreen toggles, zoom, density and toolbar changes by itself. Updates are sent on window, tab and zoom events and a 1 s poll for resizes, and only when the numbers changed. Only three numbers per window and the Strength opacity leave the browser; no URLs or titles. The options page shows whether the helper is connected.

Limit: the page area is what is measured, so a bottom findbar or docked devtools shortens it and the strip grows while they are open.

A WebExtension can only set theme colours, so it cannot change the chrome's geometry, radius or density, draw behind the page, or blur anything. The new-tab override is fixed in the manifest and cannot defer to Firefox's own page; "off" redirects to about:home. Load it with `npx web-ext run -s browser/firefox-glass` (throwaway profile) and check it with `npx web-ext lint -s browser/firefox-glass`.
