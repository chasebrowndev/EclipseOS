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

`hud/{title,body,readout}-{1x,2x}.a8` are the compositor HUD's face (annotation
HUD, titlebars, menus, select, overview, the redaction pill): Onest, proportional,
pre-rasterised into 8-bit coverage atlases by `hud/gen_atlas.c` (run
`sh hud/gen_hud.sh`; the charset is `hud/charset.txt`), compiled into
`abyss/crates/ec-abyss-render/src/hud_font.rs` (ADR 0071). The `HUD3` format
carries a per-glyph advance table (1/64 px, measured at the nominal size) beside
the cells, so layout is by measured width. Greek, maths and arrows are not in
Onest; those glyphs come from JetBrains Mono at the same size.

`trusted/jbm-{1,2,3}x.bin` are the trusted UI's fixed-cell face: JetBrains Mono
Regular then Medium, printable ASCII, one coverage byte per pixel in exact
`8s x 16s` cells, compiled into `abyss/crates/ec-abyss-render/src/glyphs.rs`
(ADR 0009). They draw everything a human must read character by character: the
untrusted well, fact values, the typed entry and the personal secret. Regenerate
with `python3 -I assets/fonts/trusted/gen_glyphs.py` (Pillow with FreeType).

`trusted/sans-{1,2,3}x.bin` (`TSN1`) are the trusted UI's compiled-in prose:
Onest Regular, Medium and SemiBold, printable ASCII, integer advances (identical
at every scale) and cells `(advance + 4) * s` wide, so layout is the same sum of
whole pixels at 1x, 2x and 3x. Never used for runtime text. Regenerate with
`trusted/gen_sans.sh`; `trusted/verify_sans.py` checks the files.

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
