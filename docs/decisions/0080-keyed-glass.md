# 0080 — Keyed glass for opaque clients
Status: accepted
Date: 2026-10-31
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0079 gave Firefox glass by fading the top strip of its opaque window to an
alpha with blur behind. Live use showed three problems:

- Text and icons in the strip fade with their background (C-21's exception).
- Only the strip is glass. Firefox draws the URL-bar results dropdown inside its
  toplevel, so the dropdown was half glass, half opaque. Menus and Firefox's own
  pages stayed opaque.
- The extension's custom new-tab page replaced Firefox Home, so users lost recent
  pages, shortcuts and Home's settings. As an extension page, it also put an
  "Extension (…)" chip in the URL bar.

Separately, a resized Firefox window lost its strip. The extension reported the
new size before Firefox committed the resized buffer, so abyss matched nothing and
dropped the strip. The extension then never resent, because its numbers had not
changed.

## Options
1. **Keep the fade.** Bring Firefox Home back themed. The strip-only glass and
   fading text stay.
2. **Keyed translucency.** The client paints its glass surfaces in exact reserved
   key colours and tells abyss what they are. abyss makes only pixels equal to a
   key translucent, at that key's alpha with the blur mode behind, and leaves
   everything else opaque.
3. **Patched Firefox** with true per-pixel alpha. Rejected in ADR 0079.

## Decision
We take **2**.

- **Shader.** abyss gains a keyed variant of the rounded-texture shader, used in
  both the live and the capture path. A texel within ±1/255 per channel of a key
  takes that key's alpha. Any other texel is drawn unchanged.
  A partly transparent texel (a client's own anti-aliased edge) is compared on
  its un-premultiplied colour, with tolerance 1.5/alpha; below alpha 0.2 it is
  drawn as is.
- **Keys and regions.**
  - Up to four keys, each with its own alpha.
  - Keys apply only where the client declares: the top strip; the whole window
    while the client reports a browser-owned page (`body`); the whole window for a
    key flagged `everywhere`; and the client's popups.
- **Collision safety.**
  - Scope comes first: a website's page area is never keyed, except for the one
    key a client flags `everywhere` (Firefox's dropdown key).
  - Keys are reserved values: the intended colour moved by one unit in one
    channel, at least two units from each other.
  - Matching is exact, so a nearby colour stays opaque.
- **`set_client_strips` v2** takes `keys[1..=4]` of `{color, opacity, everywhere?}`
  and `windows[]` of `{width, height, content, body?}`. abyss keeps each
  connection's latest request and re-matches it whenever one of the client's
  toplevels commits a new size, which fixes the resize race. The commit path does
  no work when no request exists.
  A resize commit that no stored request matches keeps the window's current
  stamp, so the window does not flash opaque before the client's new report
  arrives.
- **Rules.** `translucent-region` gains an optional `key=#rrggbb`. Without it, a
  rule keeps the uniform fade of ADR 0079.
- **The extension**
  - drops its new-tab override, so Firefox Home returns;
  - derives four keys from the user's tint (base, field, popup, panel);
  - paints every glass surface, Home included, in its key;
  - sends `body` as a boolean (it needs the `tabs` permission; the URL never leaves
    the browser).
- **Context menus.** Firefox paints them in the system `Menu` colour, which no
  theme key reaches. The add-on's `policies.json` sets `ui.menu` to the default
  popup key.

## Consequences
- C-23 amends COMP-02 §9 and COMP-13 §2. C-21's fade survives only for a rule
  without `key=`.
- A website that paints a large area in exactly the `everywhere` key colour would
  show glass there. A web page cannot know that colour unless it guesses it.
- With a custom tint, context menus no longer match a key, so they stay opaque:
  `ui.menu` is a static policy value.
- A stamped window disables direct scanout, as in ADR 0079.
- Popups are keyed by the shader path. Their blur follows the popup's own corner
  radius, measured from its texture alpha when the buffer size changes, with
  `decoration.rounding` as the fallback. Verified headlessly with Firefox's app
  menu, its context menu, the search-engine popup and the extensions panel.
- **Host-managed CSS** (amended 2026-11-01). The theme API cannot reach the
  URL-bar focus glow, the selected-tab border or Home's customize button.
  - ec-glass-host finds its Firefox profile from the parent's open profile
    files (or `-profile`).
  - It keeps `chrome/eclipse-glass.css` and `eclipse-glass-content.css` there,
    plus one `@import` line marked `eclipse-glass:managed` at the top of
    userChrome.css / userContent.css.
  - The rules are scoped to `:root[lwtheme]` / `[lwt-newtab]`, so they are inert
    without the Glass theme; `ec-glass-host --remove <profile>` deletes them.
  - policies.json turns on `toolkit.legacyUserProfileCustomizations.stylesheets`.
  - The CSS ships with the host, so it changes without an AMO re-sign. A first
    install needs one Firefox restart.

## Revisit when
- Firefox ships alpha toplevels on Wayland.
- A second app add-on needs keys with a different region shape.
