# ec-greeter-ui

The visual layer of the login greeter (ADR 0126). Draws the operator's tile
layout as glass panes, the Glances, and the login form. Not TCB, but it sits
next to the login path, so it is reviewed as if it were.

## Spec and boundaries

- Layout and tile geometry: `ec-layout` (read only). Settings: `ec-greeter`
  `Settings::parse` (read only). Glances: `ec-glance`. Palette: `ec-theme`.
- Authentication is not here. `auth::Auth` is the seam; the backend (greetd) is
  written in the main session and handed to `run`. `FakeAuth` is for previews.
- Do not depend on `ec-ui` or any desktop crate. The glass client in `glass.rs`
  is a copy of the minimum `ec-ui` needs, kept in step with `ec_protocols`.
- Do not edit `ec-greeter`, `ec-layout`, `ec-glance`, `ec-lock`, `abyss/` or any
  TCB path from here. If one of them needs a change, report it.

## Invariants

- Password text never reaches `Debug`, logs, or a `String`. `Secret` is the only
  holder; `Secret::as_bytes` is for the auth backend's conversation alone.
  `Typed` (one key's text, in a message) is redacted in `Debug`.
- The secret is moved into `Auth::submit` and never copied out. Its buffer is
  allocated once and wiped on clear, take and drop.
- Keys reach the field only while `AuthState::takes_keys()`.
- A layout that fails `ec_layout::validate`, or holds a session-only tile
  (`NowPlaying`), falls back to `Layout::default()`. Nothing else is trusted.
- View code is panic-free: sizes go through `fit_in`, which never exceeds the
  space it is given; indexes are checked. `clamp` is used only with token
  bounds, and `tokens.rs` checks every such pair is ordered at compile time.
- Glance reads run under `ec_glance::bounded` on their own thread. The UI only
  receives finished snapshots through a non-blocking send.
- Glass: the compositor draws the ground when `glass::available()`. Without it,
  `Ground` paints an opaque fallback. Focus is a brighter fill and a crisp rim.
  No glow, no gradients.

- Wallpaper (`backdrop.rs`): drawn full-bleed only when glass is NOT active (an
  opaque image would hide the compositor's glass). The fallback card shows a
  pre-blurred slice of it. The renderer draws quads, then images, then text per
  layer, so a card's shadow, frost and veil+content are separate layers.

## Tokens

Every literal size, radius and spacing lives in `tokens.rs`, or in
`ec_style_tokens` (re-exported). Colour comes from the palette only. Check with
`rg -n '0x[0-9a-fA-F]{6}|\.size\([0-9]|\.padding\([0-9]|Color \{' shell/crates/ec-greeter-ui`.

## Testing

UI runs only in the hidden headless nested abyss session. Never open a visible
winit window or touch the live session. Kill only by the pid captured at spawn.
