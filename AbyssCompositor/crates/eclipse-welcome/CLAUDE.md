<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# eclipse-welcome — the welcome screen (first-run step 0)

Read the root `CLAUDE.md` first. Governing spec: `docs/design/D-07-first-run.md`
step 0 ("the eclipse animation, greetings in ten languages, press Space").
Native Rust, not a webview: ADR 0038 (`decisions/0038-de-userland-is-native-rust.md`).

- **Not TCB.** An ordinary Wayland client (fullscreen xdg-toplevel) with no
  authority of its own. **No network. Writes nothing** — not config, not state.
  `--reconfigure` skips it entirely (that is the caller's decision, not ours).
- **It is a lib and a bin.** `eclipse_welcome::Welcome` is embeddable (the
  first-run flow can host it as step 0); `eclipse-welcome` is the standalone
  binary. The hand-off is `Message::Begin`; the bin exits 0 on it.
- **Reference:** `reference/welcome.html` is the design. The timings, easings
  and the `SPEED = 0.9` clock are ported verbatim into `timeline.rs`
  (pure functions, unit-tested). Change the reference first, then the port.
- **Greeting timing.** English holds `WELCOME_SPAN = 1.5 s` (2.5 even slots);
  the other nine get `OTHER_SPAN = 0.5 s` each, so the fly-by still ends at
  `T = 6` and the moon crossing, corona, shrink and `READY` are unmoved. The
  flight in/out is the same `FLIGHT_SECS` (0.11 s) for every word, so only
  English's near-stationary settle stretches. `timeline::slot` is the pure
  function; the reference's `WS`/`OS`/`FLIGHT` are the same numbers.
- **The eclipse becomes the O (never two Os).** The logo PNG has its own O, so
  `logo.rs` splits the asset at load into `body` (letters, the O's glow
  subtracted) and `o` (ring, disc and glow, glow rebuilt analytically and
  dithered). `body` fades in 7.4..8.7 with the O absent; the eclipse shrinks
  (`land`, 6.2..8.4, double half-cosine) onto the O measured from the asset
  (`draw::eclipse_box`, not fixed cqw constants) with the moon growing to the
  disc's size; its sun and moon stay opaque, only glow/corona fade (`halo`,
  7.7..8.7); `o_opacity` fades the real O in over it 8.1..8.7; the eclipse is
  dropped when `merged`. All pure and tested in `timeline.rs`. `READY` (8.4)
  is unchanged and the hand-off is finished before the prompt is pressable.
  `reference/welcome.html` carries the same hand-off (masked letters copy plus
  an O copy); it is an approximation (the letters copy keeps the asset's glow).
- **Input counts only once `t >= READY (8.4)`.** Space (non-repeat), a pointer
  press or a touch then starts a 700 ms fade to black, then `Begin`, once.
- **Reduced motion:** `EclipseOS_REDUCED_MOTION=1` or `--reduced-motion` pins
  `t = 20` with no keycap pulse, as the reference does.

## Deliberate deviations from the pane rules

- This is a full-screen cinematic, not a settings pane, so it does not use
  `eclipse_ui` widgets or `docs/COMPOSITION.md` anatomy. `palette.rs` is the
  only file with colour literals. Its gold is the reference's `#f4bb3c`, not
  the `ACCENT` token (`#f2c33c`); the two are 1 unit apart per channel but the
  reference is the source of truth here and this is not a themed surface.
- **Alpha compensation.** iced blends in linear light, browsers in sRGB
  (there is no `web-colors` feature). `draw.rs` remaps every translucent
  layer's alpha (`lit`, `dim`, `sheen`) so the composite lands on the sRGB
  value the browser produces. If iced ever gains `web-colors`, delete the
  remaps.
- **Crispness rules (do not regress; the owner saw blur).** The logo is
  Lanczos-resampled to the exact physical width and drawn 1:1 on a snapped
  pixel rect (never GPU-upscaled), with edge steepening on letters and ring
  only. At-rest words are `fill_text` at whole-pixel positions (glyph atlas,
  full-quality AA); outlines plus blur copies are used only while a word moves
  (`draw::is_crisp`, with a sigma dead zone). Version glyphs snap to pixels.
  Everything scales with the output scale factor.
- The CSS blur is approximated by weighted offset copies; `backdrop-filter`
  on the keycap is omitted (the ground is uniform, so it is a no-op).
- The canvas has no letter-spacing or `tnum`; the version label places glyphs
  itself.

## Fonts (nothing is vendored)

Looked up by fontconfig family name (`fc-list : family`, once at start), with a
fallback chain if a face is missing. The ISO must install:

| Family | Weight | Used for |
| --- | --- | --- |
| Cormorant Garamond | Light | Latin and Cyrillic greetings |
| Lora | Regular | version label |
| Noto Serif JP / SC / KR | Light | ようこそ / 欢迎 / 환영합니다 |

Subset the CJK faces to the glyphs used (`reference/fonts.md`: they are
10–25 MB each). Install the **static** Light cuts: iced does not select a
weight from a variable font, so a variable file renders as Regular (the dev
rig has only variable fonts, so its captures carry a little more ink than the
ISO will).

## Screenshots and visual checks: offscreen only

`eclipse-welcome --size 1920x1080 --scale 1.5 --at 5.9 --shot out.png` renders
one frozen frame **offscreen** (`iced::advanced::renderer::Headless`, wgpu, no
window, no compositor, no display) and exits; the PNG is `size * scale`
physical pixels (`--fade 0..1` overlays the exit fade). Run it with
`env -u WAYLAND_DISPLAY -u DISPLAY`. **Never open a window on the host session
to look at this** (no windowed runs, fullscreen or `grim`): the dev host is the
owner's live desktop. In windowed mode `--scale` multiplies the scale factor.
