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

Not yet (crate empty or milestone later): `config_parse`, `term_feed`, `kitty_apc`, `sixel`,
`osc_all`, `kitty_keyboard_decode`, `proto_json`, `match_lang`, `template`, `compose`.
CI smoke: 60 s per target on the pinned nightly (T-25 §8).
