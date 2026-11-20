<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# apotheosis/fuzz

The cargo-fuzz workspace (T-18 §11, T-25 §4). Non-shipping, own lockfile, own pinned
nightly (`rust-toolchain.toml`), not a member of the main workspace. It is the second
standing exception to the language rule (T-00 §3.1): only here is libfuzzer's C++
runtime built. `ci/lang-rule.sh` skips it; no shipping crate depends on it.

## Run

```
cargo install cargo-fuzz            # once
cd apotheosis/fuzz
rustup toolchain install nightly-2026-11-01 --profile minimal
CARGO_INCREMENTAL=0 nice -n 19 cargo fuzz run vt_advance -- -max_total_time=60 -jobs=1
cargo fuzz list
```

Seed corpora are in `corpus/<target>/` (small, hand-made). New findings go to
`artifacts/<target>/`; minimise with `cargo fuzz tmin` and add a regression fixture to the
owning crate before fixing. Never paste real user content into a corpus.

## Targets

| target | crate | checks |
|---|---|---|
| `vt_advance` | apoth-vt | no panic; any 1-3 way split gives an identical action stream (T-02 §5) |
| `vt_utf8_parity` | apoth-vt | bulk (SIMD) path equals a byte-at-a-time reference; W3C maximal-subpart U+FFFD (UNICODE-01) |
| `grid_ops` | apoth-grid | random edit sequences: no panic, size and history limits hold |
| `input_paste` | apoth-input | paste sanitiser: no control bytes survive, frame cannot be closed early |
| `input_key` | apoth-input | key encoder over (key, mods, kind, flags): bounded, deterministic (PERFORMANCE-32) |
| `unicode_segment` | apoth-unicode | segmenter: chunk-invariant, clusters concatenate to the input |
| `xtgettcap` | apoth-terminfo | reply grammar `ESC P [01] + r HEX(=HEX)? ESC \\` for any payload and `TN` name; reply count at most the item count |
| `vt_parse` | apoth-vt | parser: whole and chunked input give the same action stream; text runs carry no controls, strings stay under caps |
| `term_feed` | apoth-term | `Term::feed`: no panic, bounded memory, result independent of chunking |
| `osc_all` | apoth-term | every OSC number with a hostile payload: denied means no effect and no reply, replies match a closed grammar |
| `kitty_apc` | apoth-term | kitty graphics APCs, chunked: quota and caps hold, closed reply grammar, denied graphics have no effect |
| `kitty_keyboard_decode` | apoth-term | kitty keyboard requests: no panic, query reply is always `CSI ? <flags> u`, flag stacks bounded |
| `sixel` | apoth-term | sixel under every mode combination, split anywhere: caps hold, images stay cell-row slices |
| `placeholder` | apoth-term | Unicode placeholder cell scripts: quads stay inside their images |
| `iterm` | apoth-term | OSC 1337 `File=` with arbitrary file bytes through every container decoder |
| `image_formats` | apoth-term | PNG/APNG, GIF, JPEG and WebP decoder wrappers: caps hold |
| `media_apc` | apoth-term | media placement APCs against a fake player: only announced placements, per-pane cap holds |
| `config_parse` | apoth-config | arbitrary text as the user config: loads without panic, deterministic |
| `match_lang` | apoth-config | match-expression validator: no panic, within budget |
| `template` | apoth-config | template validator: no panic, within budget |
| `proto_json` | apoth-proto | control-socket line, request and server-message decoders: no panic, line limit holds, requests re-encode to themselves |
| `mux_wire` | apoth-proto | mux attach record and message decoders: no panic, bounded buffer, messages re-encode to themselves |
| `av1_obu`, `h264`, `mkv_demux`, `mp4_demux`, `helper_wire` | apoth-media | media parsers and helper wire (T-16 §6) |

`./smoke.sh [SECS] [TARGET...]` runs every target (default 60 s) for CI smoke on the pinned nightly (T-25 §8).
