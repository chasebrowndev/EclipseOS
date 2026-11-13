# Gap: VT parser strategy, vte capability audit, synchronized-update timeout

Researched 2026-11-12 from source clones under `/home/risk/.cache/apotheosis-prior-art/`
(`vte-rs` = alacritty/vte 0.15.0 at 89c9c22; `alacritty` aa5d942; `wezterm`; `ghostty`; `rio` 0.5.30; `kitty`; `foot`;
`contour` docs). Findings describe techniques in prose; no source is reproduced.

## Summary

Recommendation: **write our own Williams-state-machine parser as a small first-party crate (`apotheosis-vt`), table-driven
for the non-ground states and with a SIMD-free memchr/`str::from_utf8` fast path for ground, and use `vte` only as a
conformance oracle in tests (dev-dependency), not in the product.** Reasons, all verified in source:

1. `vte` 0.15.0 (Apache-2.0 OR MIT, deps: arrayvec, memchr, optional bitflags/cursor-icon/log/serde; all permitted) enters
   `SosPmApcString` for ESC X, ESC ^ and ESC _ and then **discards every byte**: the state routes to the shared
   "anywhere" handler, which only reacts to CAN, SUB and ESC. The `Perform` trait has no APC, SOS or PM callback. So
   kitty graphics (APC `G`), OSC-less APC protocols (ghostty glyph protocol style) and anything APC-borne are impossible
   without forking or patching vte. This confirms the claim "upstream Perform drops APC".
2. DCS streaming works in vte (`hook`, per-byte `put`, `unhook`), so sixel, XTGETTCAP, DECRQSS, tmux passthrough are
   feasible. `put` is per byte, so streaming cost is a virtual-or-inlined call per byte; no bounds or caps are applied
   by vte (the consumer must cap).
3. OSC in vte: with the default `std` feature the raw buffer is an **unbounded `Vec`**; the 1024-byte cap
   (`MAX_OSC_RAW`) applies only with `no_std` ArrayVec (excess silently dropped). OSC params are split into at most 16
   slices (`MAX_OSC_PARAMS`), the rest are not split. OSC 52, 66, 5522, 8, 1337 therefore get a whole-payload slice at ST,
   not a stream. A 100 MB OSC 52 will be buffered in full. We need a cap and a streaming hook; vte offers neither.
4. CSI params: fixed `[u16; 32]` including sub-parameters, saturating u16 arithmetic, colon (0x3A) supported with
   per-parameter sub-parameter counts (good for SGR 4:3, 38:2::r:g:b). The 32 cap and u16 are too small for some uses
   (kitty keyboard protocol and OSC-free colour reports fit; very long SGR lists truncate; `ignoring` set on overflow).
   Values above 65535 saturate, which is lossy for sixel raster attributes inside DCS params (these are not params, fine)
   and for CSI t window ops (fine).
5. C1 handling is partial. In ground, the parser splits at ESC with `str::from_utf8`; an invalid single byte in
   0x80..=0x9F is delivered via `execute`, so the host can see 0x9B as an event but vte does **not** treat it as a CSI
   introducer; in non-ground states bytes >= 0x80 are mostly ignored (only 0x9C terminates DCS passthrough; OSC ignores
   0x9C). If the escape registry wants 8-bit C1 (S8C1T, DECSCL level 2+), vte cannot do it.
6. Zero-alloc hot path: vte's ground path is allocation free (memchr for ESC, `from_utf8`, batched `print` via a
   `ground_dispatch(&str)`), but only in the sense that the parser itself does not allocate; OSC buffer is a growing Vec
   (std). Its `advance` takes slices (since 0.14) and has `advance_until_terminated` with `Perform::terminated()` for early
   stop, which is the hook alacritty uses for sync handling.
7. **Sync timeout, verified:** `alacritty_terminal` depends on vte 0.15 with the `ansi` feature; the synchronized-update
   logic lives in `vte::ansi::Processor` (not in alacritty_terminal). Constants: **150 ms** timeout
   (`SYNC_UPDATE_TIMEOUT`), **2 MiB** (0x200000) sync buffer (`SYNC_BUFFER_SIZE`), BSU/ESU recognised as the literal 8-byte
   strings `ESC [ ? 2026 h` / `ESC [ ? 2026 l`. When BSU is seen, subsequent bytes are *buffered raw* (not parsed) until ESU,
   the timeout, or buffer overflow, then replayed through the parser. That is a byte-level design, different from the
   grid-level design in ghostty (1000 ms), rio (1000 ms) and foot (1 s timerfd). So "150 ms" is true for
   Alacritty/vte only; peers use 1 s. Contour's spec says there is "no consensus" on the timeout.
8. Mode 2027 (grapheme clusters): handled above the parser in every peer (ghostty default on, rio via UAX #29 in
   `rio-unicode`, foot changelog "Support for DECSET/DECRST/DECRQM 2027"). vte's `Perform::print(char)` is per code point,
   so it cannot hand the host a grapheme without the host re-segmenting. This argues for our own ground path that emits
   `&str` runs and lets the unicode layer segment.

Peer comparison:

| Parser | Licence | APC | DCS stream | OSC cap | Colon sub-params | C1 8-bit | Notes |
|---|---|---|---|---|---|---|---|
| vte 0.15 | Apache-2.0 OR MIT | dropped | per-byte `put` | none (std) / 1024 (no_std) | yes, 32 slots u16 | partial | 95M downloads, last release 2025-02-02 |
| wezterm vtparse 0.7 | MIT | `apc_dispatch(Vec<u8>)` whole, unbounded | yes | heapless 1024 total, 64 params | `CsiParam::P(u8)` keeps `:` separators in-band, 256 slots i64 | decodes UTF-8-encoded C1 specially | last release 2025-04-19; allocates per APC |
| ghostty Parser.zig | MIT (not usable, Zig) | `apc_start/put/end` actions, per-protocol cap, 65 MiB kitty | `dcs_hook/put/unhook` | 2048 stack, 8 MiB allocating | `SepList` bitset, u16, 24 slots | table covers 0x98/0x9E/0x9F | has AFL++ fuzz harnesses |
| rio-vt | MIT | own handler | own | n/a | own | own | extracted core; sync 1000 ms |
| alacritty | Apache-2.0 | uses vte | uses vte | uses vte | uses vte | uses vte | adds sync layer |

wezterm's vtparse is the only crate that already streams APC, but it buffers the whole sequence in a `Vec` with no cap
and has no streaming sink, so it fails the "bounded streaming" requirement as is. It is MIT (allowed) and small enough to
read as prior art for table layout.

## Architecture (how it is built)

**vte**: Paul Williams' DEC parser as a hand-written `match` per state (not a table lookup); `Parser` owns a state enum,
2 intermediates, a `Params` (32 u16 + per-param subparam counts), OSC raw Vec + 16 (start,end) pairs, and a 4-byte
partial UTF-8 buffer for sequences split across reads. Ground state calls memchr for ESC, validates the run as UTF-8 and
dispatches one `print` per char via `ground_dispatch`. Actions go to a `Perform` trait: print, execute, hook, put, unhook,
osc_dispatch, csi_dispatch, esc_dispatch, terminated. The optional `ansi` module adds a higher-level `Handler` trait and a
`Processor` (with sync state: raw buffer, timeout object implementing a `Timeout` trait, so the embedding loop owns the clock).
Source: `vte-rs/src/lib.rs` (`Parser::advance`, `advance_ground`, `advance_esc`, `advance_osc_string`, `anywhere`),
`src/params.rs`, `src/ansi.rs` (`Processor::advance`, `advance_sync`, `stop_sync_internal`).

**wezterm vtparse**: state/action tables in `transitions.rs` generated from the Williams chart, actor is a `&mut dyn
VTActor` (dynamic dispatch per action). UTF-8 decode is delegated to `utf8parse` byte-at-a-time via a Utf8Sequence pseudo
state, with a special case so a UTF-8-encoded C1 (U+0080..U+009F) triggers the transition it would as an 8-bit byte.
APC start/put/end actions accumulate into a `Vec<u8>` handed over by `mem::take` at end.

**ghostty**: comptime-generated transition table from the same chart; actions are a tagged union returned by value
(at most three per byte: exit, transition, entry). Stream layer (`stream.zig`) runs SIMD UTF-8 decode (C++ via simdutf/Highway
in `src/simd/`, which we may not compile) in ground state and falls to scalar for non-ground. APC handler
(`apc.zig`) identifies the protocol from the first byte after APC (`G` = kitty graphics, glyph protocol), then streams
bytes into a per-protocol parser with its own byte cap, or into `ignore` state so unknown APCs cost zero memory.
OSC parser has a stack buffer (2048) and an allocator-backed mode capped at 8 MiB for OSC kinds that legitimately exceed it.

**rio-vt**: independent state machine in `performer/`, SIMD UTF-8 and base64 via simdutf (C++, not usable for us),
sync deadline kept on the terminal and checked by the PTY loop, grapheme segmentation from `rio-unicode` for mode 2027,
legacy iTerm2 DCS `= 1 s` / `= 2 s` sync also accepted.

**kitty**: C parser with a 1 MiB read buffer; a single escape sequence is capped at BUF_SZ/4 (256 KiB) except that the
check is deliberately lenient; input is parsed on a worker thread with a lock; a "pending input" classifier delays parsing
of small trickles (input_delay).

## Feature inventory (grouped by category)

Parser core: Williams states; ESC-in-any-state restart; CAN/SUB abort; intermediates cap 2 (vte) or 4 (ghostty);
colon sub-parameters; DEL ignore; partial UTF-8 across reads; invalid-UTF-8 replacement; C1 as 8-bit vs UTF-8-encoded.

String sequences: OSC (BEL or ST terminator, param split cap), DCS (hook/put/unhook), APC (kitty graphics, glyph protocol),
PM and SOS (always ignored, bounded skip).

Payload protocols that ride the parser: kitty graphics (APC `G`, chunked with `m=1`, chunk <= 4096 bytes base64, all but
last a multiple of 4, per docs/graphics-protocol.rst); OSC 52 clipboard; OSC 5522 (kitty clipboard: `type=read|write|wdata`
packets, base64 mime, chunks of at most 4096 bytes before base64 per docs/clipboard.rst); OSC 66 (text sizing, payload text
at most 4096 bytes per docs/text-sizing-protocol.rst, ghostty enforces `max_payload_length = 4096`); sixel (DCS q).

Synchronized output: mode 2026 with DECRQM answers 0/1/2/4 (contour spec); buffering style (alacritty: raw bytes,
2 MiB, 150 ms) versus render-hold style (ghostty/rio/foot/wezterm: parse normally, renderer shows last frame, 1 s timeout).

## Control surface (config, actions, IPC)

Proposed Apotheosis surface (derived, not peer fact):
- KDL: `vt { sync-timeout "1s"; sync-mode "hold-render"; limits { osc-max "8MiB"; osc-52-max "1MiB"; osc-66-max "4KiB"; osc-5522-max "64MiB"; apc-kitty-max "64MiB"; dcs-max "64MiB"; sos-pm-max "0" } ; c1 "utf8-only|8bit" }` plus per-protocol enable flags (`apc kitty-graphics enabled`, mirroring ghostty's per-protocol enable set and `unknown_max_bytes = 0` default).
- Actions: `vt.reset-limits-counters`, `vt.dump-parser-state`, `vt.toggle-protocol <name>`; rules: `event parser-limit-exceeded -> notify`.
- Control socket: `vt.stats` returns counters (bytes parsed, dropped APC, capped OSC, sync timeouts fired).
- Escape-sequence facts: DECSET/DECRST/DECRQM `?2026`, `?2027`; APC `ESC _ G ... ESC \`; OSC 52, 66, 5522; DCS `q` sixel.

## Performance (numbers + method)

No peer publishes a reproducible parser-throughput number we could verify from primary sources in this pass.
- vte: README/CHANGELOG claim improvement only (0.13.1 "Improve SGR performance", 0.14.0 slice API). No figures. unverified numbers.
- ghostty: ground-state SIMD decode and `nextSliceUntil` note "asking for ground costs 1% to 5% over nextSlice" (comment in
  `stream.zig`); that is a relative cost, not throughput.
- kitty: documents that its design targets the vtebench-style benchmarks; docs/performance.rst states other terminals'
  numbers "are likely to improve by 20-50%" if they adopt parallel parsing (a claim, not a measurement).
- Method to adopt: `vtebench` (cloned in the prior-art cache) plus a dedicated criterion-free microbench feeding 1 GiB of
  recorded vim, `cat` of 10 MB UTF-8, 10 MB SGR-heavy and 10 MB kitty-graphics base64 into the parser with a null sink;
  report GB/s per class. Budget proposal: ground ASCII/UTF-8 >= 2 GB/s single core with `from_utf8`+memchr, escape-heavy >= 300 MB/s,
  zero heap allocations per byte after warm-up (assert with a counting global allocator in tests).

## What Apotheosis should take

1. Own the parser crate. About 1.5-2k lines in Rust: state enum + `[ (Action, State); 256 ]` per-state tables generated by a
   `const fn`/build-free macro (no build scripts needed; tables are `const` evaluated), identical to the Williams chart.
2. Make string states **streaming with typed sinks**, not buffered dispatch. On entering OSC/DCS/APC the parser reads the
   first one to three bytes (OSC number up to `;`, APC first byte, DCS final), asks a `Registry` for a sink
   and from then on forwards chunks as `&[u8]` slices (found with memchr for ESC/BEL/CAN/SUB) with `begin(params)`,
   `chunk(&[u8])`, `end(Terminator)`, `abort(reason)`. Unknown or disabled protocols go to a zero-memory skip sink (ghostty design).
3. Per-protocol byte budgets enforced in the parser, not the sink; exceeding drops the rest and emits `abort(Overflow)`,
   which resets any half-built image/clipboard transaction. Defaults: kitty graphics 64 MiB total per APC (clients
   chunk to 4096 anyway), OSC 5522 write chunks 4096 raw (5464 base64 bytes) per packet but the sequence cap 16 KiB,
   OSC 52 1 MiB, OSC 66 4096 bytes payload, sixel DCS 64 MiB with decoded-pixel cap, generic OSC 8 KiB, SOS/PM 0 (skip).
   Note kitty chunks are separate APCs (m=1), so the 4096 limit applies per APC and the transaction cap lives in the graphics layer.
4. Stream decoders incrementally: base64 decoding in the sink as chunks arrive (reject invalid alphabet per kitty clipboard
   rules), zlib via a pure-Rust inflater (`miniz_oxide`, MIT/Zlib/Apache) with output cap, PNG via the `png` crate (MIT/Apache),
   writing into a bounded arena or a memfd handed to the sandboxed decoder helper. Never decode in the parser thread.
5. Sync design: mode 2026 handled at the terminal layer (render-hold), not by raw byte buffering. Keep parsing, keep the grid
   current, tell the renderer to present the last frame; timeout default **1 s** (peer majority), configurable, and a hard
   cap on held frames' wall time also tied to a byte volume cap (alacritty's 2 MiB idea) so a stuck app cannot hide output
   indefinitely. Also accept legacy iTerm2 DCS `= 1 s` / `= 2 s`. Answer DECRQM 2026 and 2027.
6. Ground fast path: memchr for ESC, `str::from_utf8` on the run, emit `print_str(&str)` runs, keep a 4-byte carry buffer;
   let the unicode layer segment graphemes (mode 2027) and widths. Treat C0 executes inline in the same scan (memchr2 or a
   byte-class lookup) to avoid falling out of ground for LF/CR/TAB.
7. Sub-parameters: store params as `[u32; 64]` + a colon-separator bitset (ghostty SepList idea) so `38:2::r:g:b`, `4:3`,
   and kitty keyboard `u` sequences are lossless; saturate at u32; flag `ignoring` on overflow.
8. C1: default UTF-8 mode where only 7-bit ESC forms introduce sequences and a UTF-8-encoded U+0080..U+009F is treated as
   data per ECMA-35 conformance; optional `c1 "8bit"` mode when S8C1T is requested (wezterm shows the special-casing needed).
9. Fuzzing without C: **`cargo-fuzz` uses libFuzzer, whose runtime is C++ built by `libfuzzer-sys`'s build script**; it is a
   dev tool outside the shipped build but violates the strict reading of "no C/C++ compiled by our build". Use instead
   (a) `proptest`/`arbtest` structure-aware generators in the normal test suite (pure Rust, MIT/Apache; `arbtest` unverified licence: check),
   (b) a deterministic differential test against `vte` as dev-dependency on 7-bit streams without APC (assert identical CSI/OSC/ESC dispatch),
   (c) a corpus replay harness (ghostty ships `replay-crashes` scripts), and (d) optionally run cargo-fuzz in a separate CI job
   that is not part of the product build. Invariants fuzzed: never panics, never allocates beyond caps, parser returns to ground on
   CAN/SUB/ESC, any split of the input at any byte boundary yields identical actions (chunk-boundary property test).
10. Conformance: esctest2 and vttest corpora (esctest2 is in the prior-art cache) as integration tests.

## What it should avoid

- Adopting vte as-is or forking it: APC dropped, unbounded OSC (std), no streaming OSC/APC, C1 gaps, u16/32 param cap, per-char print.
  A fork would diverge from the 95M-download upstream and still need a rewrite of three states.
- Using wezterm vtparse for the product: whole-sequence `Vec` buffering for APC, dynamic-dispatch actor, no caps.
- Alacritty-style raw-byte buffering for synchronized update: the parser cannot report errors or answer queries in the held
  region, DECRQM and DA replies within a frame stall, and 150 ms cuts off legitimate slow frames; the 2 MiB buffer is a copy.
- SIMD via C/C++ libraries (simdutf in rio, Highway in ghostty); use `memchr` (pure Rust, Unlicense OR MIT) and `core::str`/`simdutf8`
  alternatives only if pure Rust (unverified for `simdutf8` licence, MIT/Apache expected).
- Per-byte virtual `put` for large payloads; per-protocol sinks must take slices.
- Letting any string state buffer without a cap; letting unknown APC/PM/SOS retain memory.
- Treating the 4096 chunk limit as the cap; clients may send an unchunked APC and the terminal must still bound it.

## Sources

Source findings (path + function):
- alacritty/vte `src/lib.rs`: `Parser::advance`, `advance_ground`, `advance_esc` (0x58/0x5E/0x5F to SosPmApcString), `change_state`
  (SosPmApcString uses `anywhere`), `advance_osc_string`, `action_osc_put` (std Vec unbounded), `action_paramnext` (saturating u16),
  `advance_dcs_passthrough` (0x9C unhook); `src/params.rs` (`MAX_PARAMS = 32`, subparams); `CHANGELOG.md` (0.14.0 slice API, 0.15.0).
- alacritty/vte `src/ansi.rs`: `SYNC_UPDATE_TIMEOUT` 150 ms, `SYNC_BUFFER_SIZE` 2 MiB, `BSU_CSI`, `ESU_CSI`, `Processor::advance`,
  `advance_sync`, `stop_sync_internal`.
- alacritty/alacritty `alacritty_terminal/Cargo.toml` (vte 0.15.0, features std+ansi), `term/mod.rs` (SyncUpdate private mode report).
- wezterm `vtparse/src/lib.rs`: `VTActor::apc_dispatch`, `VTParser::action` ApcStart/ApcPut/ApcEnd, `next_utf8`, `CsiParam`; `vtparse/Cargo.toml`; `mux/src/lib.rs` (SynchronizedOutput frame start/end).
- ghostty `src/terminal/Parser.zig` (`MAX_PARAMS = 24`, `MAX_INTERMEDIATE = 4`, apc_start/put/end, `Sep`), `parse_table.zig` (0x98/0x9E/0x9F),
  `apc.zig` (`Handler`, `defaultMaxBytes` kitty 65 MiB), `osc.zig` (`MAX_BUF` 2048, `MAX_ALLOCATING_BUF` 8 MiB), `osc/parsers/kitty_text_sizing.zig`
  (`max_payload_length` 4096), `osc/parsers/kitty_clipboard_protocol.zig`, `stream.zig` (`nextSlice`, SIMD path), `termio/Thread.zig` (`sync_reset_ms = 1000`),
  `terminal/modes.zig` (2026, 2027), `test/fuzz-libghostty/README.md` (AFL++ targets osc, parser, stream).
- rio `rio-vt/src/crosswords/mod.rs` (`SYNC_UPDATE_TIMEOUT` 1000 ms, `sync_update_deadline`), `performer/handler.rs` (iTerm2 DCS sync), `Cargo.toml` (simdutf, rio-unicode for 2027).
- foot `terminal.c` (app sync updates timerfd, 1 s `tv_sec`), `csi.c` (DECSET 2026, DECRQM), `CHANGELOG.md` (2027 support).
- kitty `kitty/vt-parser.c` (`BUF_SZ` 1 MiB, `MAX_ESCAPE_CODE_LENGTH`, pending-input classifier); kitty pending-mode timeout value not located: unverified.
- Docs: https://sw.kovidgoyal.net/kitty/graphics-protocol/ (local `docs/graphics-protocol.rst`), https://sw.kovidgoyal.net/kitty/clipboard/,
  https://sw.kovidgoyal.net/kitty/text-sizing-protocol/, https://vt100.net/emu/dec_ansi_parser (Williams chart),
  https://github.com/contour-terminal/contour/blob/master/docs/vt-extensions/synchronized-output.md (timeout "no consensus"),
  https://gitlab.com/gnachman/iterm2/-/wikis/synchronized-updates-spec, https://crates.io/crates/vte and https://crates.io/crates/vtparse (API metadata, 2026-11-12).
- Unverified: parser throughput numbers for any peer; `arbtest`/`simdutf8` licence strings; kitty's pending-mode timeout; whether Alacritty's 150 ms has ever been made configurable.
