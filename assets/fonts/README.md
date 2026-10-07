# Vendored fonts

The TTF families are [SIL Open Font Licence 1.1](https://openfontlicense.org/) —
a data asset, not linked code, so they do not appear in `cargo deny`'s licence
graph. The licence text ships beside them, which is all OFL-1.1 §2 asks.

| file | source |
|---|---|
| `InstrumentSans-{Regular,Medium,SemiBold}.ttf` | instanced from Google Fonts' `InstrumentSans[wdth,wght].ttf` at `wght=400/500/600, wdth=100` |
| `JetBrainsMono-{Regular,Medium}.ttf` | JetBrains Mono, upstream statics |

`LICENSE-Spleen.txt` (BSD-2-Clause) covers the Spleen 2.1.0 8x16 glyphs that
`abyss/crates/ec-abyss-render/src/font.rs` carries as a table, compiled in
rather than loaded (ADR 0009). No Spleen file is
shipped here; the table header says how to regenerate it from upstream's BDF.

`hud/{title,body,readout}-{1x,2x}.a8` are the annotation HUD's face: JetBrains
Mono pre-rasterised into 8-bit coverage atlases by `hud/gen_atlas.c`, compiled
into `abyss/crates/ec-abyss-render/src/hud_font.rs` (ADR 0071). Each file is an
8-byte header (`HUD1`, cell width, cell height, advance, left margin) and 97
cells: printable ASCII, then U+00D7 and U+00B7. Regenerate with the command at
the top of `gen_atlas.c`; it reads the two JetBrains Mono statics above.

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

To regenerate the Instrument Sans statics:

```
fonttools varLib.instancer InstrumentSans[wdth,wght].ttf wght=<400|500|600> wdth=100 \
    --output InstrumentSans-<Regular|Medium|SemiBold>.ttf
```

then set name IDs 2/4/6 to the subfamily — the instancer leaves all three
saying "Regular", which is cosmetic but misleads anyone inspecting the file.
