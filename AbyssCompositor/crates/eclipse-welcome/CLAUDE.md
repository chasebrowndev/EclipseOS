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
weight from a variable font, so a variable file renders as Regular.

## Screenshots

`eclipse-welcome --size 1920x1080 --at 5.9 --shot out.png` renders one frozen
frame and exits (`--fade 0..1` overlays the exit fade). It needs a Wayland
display but no compositor of ours.
