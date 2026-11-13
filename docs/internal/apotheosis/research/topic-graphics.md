# Topic dossier: terminal image protocols (graphics)

Scope: kitty graphics, sixel, iTerm2 OSC 1337, terminal-wg proposals, security limits, Rust decoder crates.
Method: official docs fetched 2026-11-12; source read from shallow clones in /home/risk/.cache/apotheosis-prior-art (kitty, foot, ghostty, wezterm). Source findings are described as technique only. Anything not verified is marked "unverified".

## Summary

- Kitty graphics is the protocol to implement first and completely. It is the only one with image ids, placements, z-order, animation, relative placement, a storage quota, and unicode placeholders (the only mechanism that survives tmux/zellij-style multiplexing, since the image becomes ordinary text cells).
- Sixel is required for compatibility (lsix, chafa, mpv --vo=sixel, img2sixel, gnuplot). It is a cell-attached bitmap, not an object model. Decode it to RGBA, store it in the same image store as kitty images, and treat it as a placement bound to cells.
- iTerm2 OSC 1337 File= is small to implement once an image store exists (imgcat, many tools emit it). Include inline=1 only; refuse the download (inline=0) half.
- The terminal-wg "good image protocol" effort never produced a ratified spec (unverified: the issue search for terminal-wg/specifications failed in this session; no spec text was read). Do not design around it; kitty is the de facto standard.
- All three decoders feed one pipeline: decode, validate against limits, upload to a GPU texture atlas or per-image texture, place by cell coordinates. Security comes from hard limits (dimension, bytes, total quota, file-medium restrictions) applied before allocation.
- Rust crates under the licence list are sufficient: png, gif, zune-jpeg (or jpeg-decoder), image-webp, plus own sixel decoder (small) or icy_sixel. No C needed.

## Architecture (how it is built)

### kitty (reference implementation, kitty/kitty/graphics.c)
- A per-screen GraphicsManager holds images (by id and by "image number" I), each with placements, animation frames, and a byte counter. A main and an alt-screen manager exist, so alt-screen images do not leak into scrollback.
- Quota: DEFAULT_STORAGE_LIMIT is 320 MiB; when exceeded, apply_storage_quota sorts images by last-access time and removes oldest until under the limit (images not being displayed are evicted first). Animation frames live in a disk-backed cache with a limit 5x the storage limit (graphics.c storage_limit * 5, error ENOSPC).
- Hard caps: MAX_IMAGE_DIMENSION 10000 px per side; MAX_DATA_SZ 400,000,000 bytes (4 * 100,000,000) for compressed/PNG buffers; error codes EFBIG, EINVAL, ENOSPC. Parent-chain depth limit 8 (PARENT_DEPTH_LIMIT) with ETOODEEP on deeper or cyclic chains.
- File-descriptor care: a comment in graphics.c notes opening many files can exhaust the process fd limit; file/shm media are opened, read, and closed in one step.
- Placements are stored in screen coordinates and scroll with text; on scroll the manager shifts or removes placements that leave the scrollback, and rows are reference-counted by image so deleting scrollback frees data.

### ghostty (Zig, src/terminal/kitty/graphics_*.zig)
- Split into command parse, image load (LoadingImage), storage, exec, render placement, unicode, animation. total_limit default 320 * 1000 * 1000 bytes; limit 0 disables the protocol; evicts oldest on overflow (graphics_storage.zig).
- Transmission mediums are gated by an explicit Limits struct (file, temporary_file with required directory, shared_memory); the default "direct" limits disable all three. This is the cleanest security model seen: mediums are capability flags set by the embedder (graphics_image.zig Limits).
- Unicode placeholder: placeholder codepoint U+10EEEE; a row of placeholder cells is rebuilt into "runs" (IncompletePlacement with append/canAppend) so adjacent cells with consistent id/row/col merge into one drawn rectangle; image id comes from fg colour (24-bit) plus optional third-diacritic high byte; placement id from underline colour (graphics_unicode.zig). The diacritic table is binary-searched (getIndex).

### foot (C, sixel.c, csi.c)
- Sixel decoded straight into a pixel buffer on a state machine; images are chopped into per-cell-row slices so scrolling and partial overwrite only need slice bookkeeping. Max width/height 10000 px (SIXEL_MAX_WIDTH/HEIGHT); max colour registers 1024 (SIXEL_MAX_COLORS), adjustable by XTSMGRAPHICS.
- Private vs shared palette via DEC private mode 1070 (use_private_palette); display (scrolling) mode via mode 80 (sixel.scrolling = !enable), mode 8452 positions cursor to the right of the image.
- P2 handling: P2=1 means transparent background (empty pixels stay transparent, image flagged non-opaque); P2=0 or 2 fills with palette[0]/background. Trailing fully transparent rows are trimmed only when transparent; opaque images keep the raster-attribute height (sixel.c around the "Strip trailing fully transparent rows" logic).
- Default aspect ratio is 2:1 (P1) per DEC, with pan/pad derived from P1; raster attributes (" Pan;Pad;Ph;Pv) resize the canvas, and Ph/Pv larger than max are rejected (sixel.c ~line 1945).
- XTSMGRAPHICS reply for colour count: CSI ? 1 ; 0 ; <n> S.

### wezterm (Rust, term/src/terminalstate/image.rs)
- Single ImageCell model for all protocols: sixel, iTerm2 and kitty all produce an ImageData (Arc, content-hash keyed in image_cache so duplicate images share a texture) and ImageCell tiles with texture coordinates, one per cell. Hard cap MAX_IMAGE_SIZE 100,000,000 bytes for iTerm2 inline data. Cell-attached model means scrolling is free, but z-order/under-text and sub-cell offsets are limited versus kitty.

### Design consequence
Two data models exist: (a) object model (kitty: id + placement list in screen coords) and (b) cell-attached tile model (wezterm, sixel in most terminals). Recommended: one image store with refcounted decoded images and a placement list anchored to buffer line handles (survives scroll, reflow policy chosen explicitly), with sixel/iTerm2 creating anonymous placements.

## Feature inventory (grouped)

### A. Kitty graphics, commands (APC `ESC _ G <keys> ; <base64> ESC \`)
- Actions a=: T (transmit and display), t (transmit only), p (put/place), d (delete), f (animation frame load), a (animation control), c (compose frames), q (query; no store). Newer: a=q with e=<action> feature query (kitty 0.50.0, docs).
- Transmission t=: d direct, f file, t temp file (must be in temp dir and path contains `tty-graphics-protocol`, deleted after read), s POSIX shm (unlinked after read). All read failures must return the same error (EBADF) to avoid leaking file existence.
- Formats f=: 32 RGBA (default), 24 RGB, 100 PNG; 8 and 16 indexed (0.50.0, palette indices map to terminal palette including default fg/bg/cursor slots 248-255).
- o=z zlib compression; S/O size and offset for file/shm partial reads; m=1/0 chunking (4096-byte base64 chunks, multiples of 4; only first chunk carries keys).
- Ids: i image id (u32), I image number (terminal assigns id and reports it), p placement id; q=1/2 suppress OK/error replies. Reply format `ESC _ G i=<id> ; OK|ENOENT:..|EBADF:..|EINVAL:..|ENOSPC:..|EFBIG:.. ESC \`.
- Layout: x,y,w,h source rect; X,Y sub-cell pixel offsets; c,r scale to cell rect (aspect preserved if one given); z z-index (negative = under text; below -1,073,741,824 under non-default backgrounds); C=1 do not move cursor.
- Delete d=: a/A all visible, i/I by id (+p), n/N by number, c/C at cursor, f/F animation frames, p/P at cell, q/Q cell+z, r/R id range, x/X column, y/Y row, z/Z z-index. Lowercase keeps data; uppercase frees data.
- Relative placement: P (parent image), Q (parent placement), H,V cell offsets; depth >= 8 required, children die with parent; virtual placements can be parents but not children.
- Unicode placeholders: U=1 creates virtual placement; draw via U+10EEEE with combining diacritics (row, col, optional high id byte) from the published rowcolumn-diacritics table; id in fg colour, placement id in underline colour; omitted diacritics inherit from the cell to the left under stated conditions. Virtual placements deleted only by i/I/r/R/n/N.

### B. Kitty animation
- a=f loads frames: r edit frame (else append), c base frame copy, x,y,s,v partial rectangle, z gap ms (default 40, negative = gapless), X composition (1 overwrite, default alpha blend), Y background RGBA.
- a=a control: s=1 stop, 2 run waiting for frames, 3 loop; v loop count (1 infinite, n = n-1 loops); c current frame; r,z per-frame gap edit.
- a=c composes rect from frame r to frame c with offsets and blend mode.
- kitty guards composition recursion (coalesced frame depth) and caps animation memory via the 5x on-disk cache.
- Animation is not supported for indexed formats.

### C. Sixel (DCS `P1;P2;P3 q <data> ST`)
- Data chars 0x3F-0x7E = 6 vertical pixels; `!n` repeat; `#n` select register; `#n;2;r;g;b` RGB (0-100%); `#n;1;h;l;s` HLS definition (hue angle origin differs, blue at 0 deg; unverified in this session); `$` carriage return (overwrite same band with another colour); `-` new line (down 6px); `"Pan;Pad;Ph;Pv` raster attributes.
- Edge cases: colour register overflow (index >= palette size: foot clamps to palette_size-1), registers redefined mid-image, P2 transparency, default 2:1 aspect, raster attributes absent (canvas grows), width cap, height cap, images wider than the screen, unterminated sequence, repeat counts huge (cap them).
- Modes: DECSDM `CSI ? 80 h/l` (display/scrolling mode: set = no scroll, image clipped at bottom, cursor stays home; reset = scrolls), `?1070` private colour registers per graphic, `?8452` cursor right of image.
- Queries: XTSMGRAPHICS `CSI ? Pi ; Pa ; Pv S` for colour registers (Pi=1) and geometry (Pi=2); DA1 attribute 4 advertises sixel.
- Cell-size queries clients rely on: `CSI 14 t`, `CSI 16 t`, `CSI 18 t`.

### D. iTerm2 inline images (OSC 1337)
- `ESC ] 1337 ; File = k=v;k=v : <base64> BEL` (ST allowed). Keys: name (base64), size, width, height (units N cells, Npx, N%, auto), preserveAspectRatio (default 1), inline (1 display; 0 save to Downloads, which we must not implement), doNotMoveCursor, type (not documented on the fetched page).
- Multipart for tmux: MultipartFile=, FilePart= chunks, FileEnd; iTerm2 caps each chunk at 1,048,576 bytes (docs).
- Image format is sniffed from file content (any decodable format incl. GIF animation, PDF in iTerm2; for us: PNG/JPEG/GIF/WebP only).

### E. Other
- terminal-wg: no ratified image spec found (unverified). Treat as watch item.
- DEC ReGIS/Tektronix: out of scope.

## Control surface (config, actions, IPC)

Proposed KDL (names are proposals):
```
graphics {
    kitty true
    sixel true
    iterm2 true
    storage-limit "320MiB"        // per screen buffer, kitty default
    animation-cache-limit "1600MiB"
    max-dimension 10000
    max-bytes "400MB"
    media { direct true; file "prompt"; temp-file true; shared-memory true }  // false|true|"prompt"
    sixel-colors 1024
}
```
Actions (shared vocabulary): `graphics-clear` (delete visible/all placements), `graphics-toggle-animation`, `graphics-save-image` (export under cursor), `graphics-open-image`, `graphics-copy-image`, `graphics-stats` (palette overlay with quota use).
Rules/events: `image-received` (event, fields: protocol, bytes, medium) so rules can prompt or deny; `image-denied`.
IPC (JSON socket): `graphics.list` (ids, sizes, bytes, pane), `graphics.delete {id|all|pane}`, `graphics.stats`, `graphics.set-limit`. CLI: `apo graphics ls|rm|stats`.
Settings GUI: toggles per protocol, limit sliders, per-medium policy.

## Performance (numbers + method)

No benchmark numbers were measured here (no builds allowed). Published figures:
- kitty: 320 MiB default quota per buffer, animation disk cache 5x; MAX_IMAGE_DIMENSION 10000 (source, graphics.c).
- 10000x10000 RGBA = 400 MB, i.e. the 10000-px dimension cap alone does not bound memory; check width*height*4 against max-bytes before allocating (kitty compares data size to MAX_DATA_SZ 400 MB).
- Method recommendations for Apotheosis benches: (1) throughput of `kitten icat`-style 4096-byte chunk streams, MB/s through parser; (2) decode latency for 4K PNG/JPEG; (3) sixel decode time for 1920x1080 at 256 colours (img2sixel output); (4) frames/s of 60-frame GIF animation with texture upload only for changed rect; (5) scroll with 100 live placements, GPU time per frame.
- Techniques: decode off the main thread (pool, return by channel); upload partial-frame rects via queue.write_texture; keep images as separate textures with atlas only for small ones; hash-dedupe identical payloads (wezterm image_cache).

## What Apotheosis should take

1. Full kitty graphics with all actions, all deletes, relative placements, unicode placeholders, animation, a=q feature query.
2. Medium policy flags (ghostty Limits model) with default direct-only and a prompt-on-use mode for file/shm.
3. Storage quota with LRU eviction, separate animation cache quota, per-screen managers.
4. Sixel decoder with registers, HLS, raster attrs, modes 80/1070/8452, XTSMGRAPHICS, DA1 advert.
5. iTerm2 inline=1 with units and multipart.
6. Single image store plus placement list; sixel/iTerm2 use it.
7. Content-hash dedupe and refcounting.
8. Decode in a sandboxed worker (ties to the existing sandboxed decoder helper plan) so malformed PNG/JPEG cannot touch the terminal process.
9. Image export/copy and a settings page showing quota use.

## What it should avoid

- iTerm2 download mode (inline=0) and any file write from escape sequences.
- Following symlinks to special files, /proc, /sys, /dev for t=f; reading outside temp dir for t=t; different error text per failure (existence oracle).
- Trusting declared dimensions before checking byte budget; unbounded repeat counts in sixel; unbounded chunk accumulation.
- Cell-tile-only model (loses sub-cell offsets and z-order).
- Sending any reply to unsolicited queries from untrusted data without rate limits.
- Designing around terminal-wg proposals.
- Logging image bytes or file paths content (human input rule: no content logs).

## Sources

- https://sw.kovidgoyal.net/kitty/graphics-protocol/ (fetched; the placeholder section body was not fully read, see unverified notes)
- https://iterm2.com/documentation-images.html
- kitty source: kitty/kitty/graphics.c (apply_storage_quota, load_image_data_from_file, handle_add_command, MAX_* defines)
- ghostty source: src/terminal/kitty/graphics_image.zig (Limits), graphics_storage.zig (total_limit, setLimit), graphics_unicode.zig (IncompletePlacement, getIndex)
- foot source: sixel.c (sixel_init, resize, trimming), sixel.h (limits), csi.c (modes 80, 1070), terminal.c
- wezterm source: term/src/terminalstate/image.rs (MAX_IMAGE_SIZE, image_cache)
- crates.io API (licence/version, queried 2026-11-12)

### Rust decoder crates (licence check)
| crate | version | licence | note |
|---|---|---|---|
| png | 0.18.1 | MIT OR Apache-2.0 | safe, streaming; set Limits |
| gif | 0.14.2 | MIT OR Apache-2.0 | frame-by-frame animation; uses weezl |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib | fast, safe |
| jpeg-decoder | 0.3.2 | MIT OR Apache-2.0 | alternative; maintenance lower (unverified) |
| image-webp | 0.2.4 | MIT OR Apache-2.0 | pure Rust WebP, lossy+lossless+animation (animation support unverified) |
| image | 0.25.10 | MIT OR Apache-2.0 | aggregator; Limits API; heavy feature surface |
| weezl | 0.2.1 | MIT OR Apache-2.0 | LZW |
| fast_image_resize | 6.1.0 | MIT OR Apache-2.0 | SIMD scaling (unverified licence of deps) |
| icy_sixel | 0.7.0 | MIT OR Apache-2.0 | sixel encode/decode, young crate (unverified quality) |
| sixel-image | 0.2.1 | MIT | zellij's; weak edge-case coverage (unverified) |
