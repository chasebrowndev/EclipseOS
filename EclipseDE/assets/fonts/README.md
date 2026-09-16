# Vendored fonts

Both families are [SIL Open Font Licence 1.1](https://openfontlicense.org/) —
a data asset, not linked code, so they do not appear in `cargo deny`'s licence
graph. The licence text ships beside them, which is all OFL-1.1 §2 asks.

| file | source |
|---|---|
| `InstrumentSans-{Regular,Medium,SemiBold}.ttf` | instanced from Google Fonts' `InstrumentSans[wdth,wght].ttf` at `wght=400/500/600, wdth=100` |
| `JetBrainsMono-{Regular,Medium}.ttf` | JetBrains Mono, upstream statics |

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
