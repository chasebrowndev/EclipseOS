# Eclipse OS — visual style spec

A dark, warm-black desktop OS settings UI with balanced glassmorphism and a single
yellow accent. Use this as art direction; do not copy any specific screen.

## Colour
- Window / desktop base: #0b0906 (warm near-black — never blue-grey)
- Secondary surfaces behind glass: #12100b, #1a1712, #1c1913
- Glass panels: rgba(255,255,255,.035) to .06 fill, backdrop-filter: blur(28-56px) saturate(120-170%)
- Panel borders: 1px solid rgba(255,255,255,.08-.16)
- Top edge highlight: inset 0 1px 0 rgba(255,255,255,.07-.2)
- Outer shadow: 0 24-40px 60-90px -20px rgba(0,0,0,.85-.92)
- Accent: #f2c33c. Tints: rgba(242,195,60,.09) fill, .24-.32 border, #f5cf5c for text
- Text: #fff primary, rgba(255,255,255,.64) secondary, rgba(255,255,255,.4) tertiary
- Neutral swatch grey: #96918a (warm, not #8f8f96)

## Accent discipline
Yellow marks state and one live value per screen: the active nav item, the selected
option, the current slider fill, an "in use" status. Never decorative, never two
competing yellow areas in one pane, no gradients of it.

## Type
- Instrument Sans (400/500/600) for UI labels and titles; -0.01em tracking on headings
- JetBrains Mono (400/500) for all data, units, paths, timestamps, device identifiers,
  and small uppercase section labels (9.5-10px, letter-spacing .12-.2em)
- Sizes: pane title 24px/600, card title 12.5-13.5px/500, body 12-13.5px/400,
  mono data 10.5-12px, big numbers 19-38px

## Structure
- Window: 1120x720, radius 16px, 1px light border. No traffic-light dots — this is a
  window manager, so the titlebar is either absent or a thin label row.
- Left sidebar 214px: rgba(0,0,0,.4) + blur(30px), text nav items each with a small
  rounded placeholder icon square; active item = translucent fill + 3px yellow left bar
- Sidebar footer: small mono status block (uptime, daemon version, device count)
- Content column: 26/30px padding, 18px gap between blocks
- Header row: title + one-line subtitle (containing one yellow phrase) on the left,
  1-3 pill controls or a toggle on the right
- Then a hero block unique to the pane (arrangement canvas, level meter, focus banner,
  charge bar + 24h histogram, status metric grid)
- Then one or two inset list panels: header row, hairline rows
  (rgba(255,255,255,.05-.06)), label left, mono value right
- Radii: window 16, cards 13-14, inset chips 9-11, pills 99px

## Controls
- Toggle: 44x25 pill, yellow with a #0b0906 knob when on; rgba(255,255,255,.13) with a
  translucent white knob when off
- Slider: 4-6px track rgba(255,255,255,.12), yellow fill, 16-18px yellow round knob
- Radio: 11px circle, yellow ring + yellow dot when selected
- Segmented choice: pill buttons; selected gets yellow tint fill, yellow border, yellow text
- Bar charts: flat rects, 2-3px top radius, rgba(255,255,255,.13) for history,
  #f2c33c only for the current or peak bar

## Content rules
- Fictional but plausible system data: device names, config paths (~/.config/wm/theme.conf),
  daemon versions, dBm, Hz, W, GB, timestamps. No brand or trademark names.
- No card-in-card-in-card; one level of inset inside a glass panel is the limit
- No lorem, no decorative radial glow blobs, no emoji, no hand-drawn SVG icons —
  small rounded rects stand in for icons
- Every pane shows real state somewhere (what is connected, in use, or queued)

## What to build
One settings pane per screen at 1120x720, sidebar + detail, inline styles only.
Candidate panes: appearance, network, display, sound, input, notifications, power,
privacy, storage, keybinds, autostart, accounts.
