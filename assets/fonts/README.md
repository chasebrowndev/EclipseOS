**English** | [Español](README.es.md)

# Vendored fonts

The TTF families are [SIL Open Font Licence 1.1](https://openfontlicense.org/) —
a data asset, not linked code, so they do not appear in `cargo deny`'s licence
graph. The licence text ships beside them, which is all OFL-1.1 §2 asks.

| file | source |
|---|---|
| `Onest-{Light,Regular,Medium,SemiBold,Bold,ExtraBold}.ttf` | the shell's one face: instanced by `gen_onest.py` from Google Fonts' `Onest[wght].ttf` at `wght=300..800`, `tnum` digits made default. Shell embeds Regular/Medium/SemiBold; the welcome screen adds Light; Bold/ExtraBold are the logo wordmark's (`ec-welcome/assets/gen_logo.py`) |
| `InstrumentSans-{Regular,Medium,SemiBold}.ttf` | no longer used by the shell; kept only because `apotheosis/` (`apoth-font`) still embeds them |
| `JetBrainsMono-{Regular,Medium}.ttf` | JetBrains Mono, upstream statics. No longer a UI face: only the source the HUD and trusted bitmap atlases are rasterised from |

`LICENSE-Spleen.txt` (BSD-2-Clause) covers the Spleen 2.1.0 8x16 glyphs that
`abyss/crates/ec-abyss-render/src/font.rs` carries as a table, compiled in
rather than loaded (ADR 0009). No Spleen file is
shipped here; the table header says how to regenerate it from upstream's BDF.

`hud/{title,body,readout}-{1x,2x}.a8` are the annotation HUD's face: JetBrains
Mono pre-rasterised into 8-bit coverage atlases by `hud/gen_atlas.c`, compiled
into `abyss/crates/ec-abyss-render/src/hud_font.rs` (ADR 0071). Each file is an
8-byte header (`HUD2`, cell width, cell height, advance, left margin) and 435
cells: printable ASCII, then U+00D7 and U+00B7, then 338 codepoints ascending
(Latin-1, Latin Extended-A, Greek, math and punctuation; the `EXTENDED` table
in `gen_atlas.c`). The last 11 are composed cells in U+E000..U+E00A: a base
letter with a combining circumflex or macron over it (the `COMPOSED` table, ADR
0083 amendment). The annotation HUD only; the trusted face stays ASCII.
Regenerate with the command at the top of `gen_atlas.c`; it reads the two
JetBrains Mono statics above and fails on any codepoint a face lacks.

`trusted/jbm-{1,2,3}x.bin` are the trusted UI's face (prompts, the emergency
panel, the commit slot card): JetBrains Mono Regular then Medium, printable
ASCII, one coverage byte per pixel in exact `8s x 16s` cells, compiled into
`abyss/crates/ec-abyss-render/src/glyphs.rs` (ADR 0009). Regenerate with
`python3 -I assets/fonts/trusted/gen_glyphs.py` (Pillow with FreeType); the
output is byte-identical on rerun.

Static instances rather than the variable font on purpose: iced resolves a face
by family and weight, and a variable font handed to it whole is one face at its
default weight — every label would come out at 400 and the design's 500/600
distinctions would silently vanish.

To regenerate the Onest statics (needs fontTools):

```
python3 -I assets/fonts/gen_onest.py path/to/'Onest[wght].ttf' assets/fonts
```

It instances each weight, sets name IDs 1/2/4/6 (family `Onest`, subfamily the weight name, which is
what iced's `Family::Name("Onest")` + `Weight` resolves against) and repoints the digits' cmap
entries at the font's `tnum` glyphs (`zero.tf`..`nine.tf`, 672/1000 em in every weight). Onest has
the `tnum` feature, but iced cannot enable OpenType features, so tabular figures are baked in as
the default instead. Onest covers Latin, Latin Extended and Cyrillic; no Greek, CJK, Arabic or Indic.
