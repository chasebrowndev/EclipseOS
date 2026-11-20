[English](README.md) | **Español**

# Fuentes incluidas

Las familias TTF están bajo la [SIL Open Font Licence 1.1](https://openfontlicense.org/) —
un recurso de datos, no código enlazado, por lo que no aparecen en el grafo de licencias de `cargo deny`.
El texto de la licencia se distribuye junto a ellas, que es todo lo que pide OFL-1.1 §2.

| archivo | origen |
|---|---|
| `InstrumentSans-{Regular,Medium,SemiBold}.ttf` | instanciadas a partir de `InstrumentSans[wdth,wght].ttf` de Google Fonts en `wght=400/500/600, wdth=100` |
| `JetBrainsMono-{Regular,Medium}.ttf` | JetBrains Mono, estáticas originales del upstream |

`LICENSE-Spleen.txt` (BSD-2-Clause) cubre los glifos 8x16 de Spleen 2.1.0 que
`abyss/crates/ec-abyss-render/src/font.rs` incluye como una tabla, compilada
en lugar de cargada (ADR 0009). Aquí no se
distribuye ningún archivo de Spleen; el encabezado de la tabla explica cómo regenerarla a partir del BDF del upstream.

`hud/{title,body,readout}-{1x,2x}.a8` son la tipografía del HUD de anotaciones: JetBrains
Mono prerrasterizada en atlas de cobertura de 8 bits mediante `hud/gen_atlas.c`, compilados
en `abyss/crates/ec-abyss-render/src/hud_font.rs` (ADR 0071). Cada archivo es un
encabezado de 8 bytes (`HUD2`, ancho de celda, alto de celda, avance, margen izquierdo) y 430
celdas: ASCII imprimible, luego U+00D7 y U+00B7, luego 333 puntos de código ascendentes
(Latin-1, Latin Extended-A, griego, matemáticos y puntuación; la tabla `EXTENDED`
en `gen_atlas.c`). Las últimas 11 son celdas compuestas en U+E000..U+E00A: una letra
base con un circunflejo o macrón combinante encima (la tabla `COMPOSED`, enmienda de ADR
0083). Solo para el HUD de anotaciones; la tipografía confiable sigue siendo solo ASCII.
Regenera con el comando al principio de `gen_atlas.c`; lee las dos estáticas de
JetBrains Mono indicadas arriba y falla ante cualquier punto de código que falte en una tipografía.

`trusted/jbm-{1,2,3}x.bin` son la tipografía de la interfaz confiable (avisos, el panel de
emergencia, la tarjeta de la ranura de commit): JetBrains Mono Regular y luego Medium, ASCII
imprimible, un byte de cobertura por píxel en celdas exactas de `8s x 16s`, compiladas en
`abyss/crates/ec-abyss-render/src/glyphs.rs` (ADR 0009). Regenera con
`python3 -I assets/fonts/trusted/gen_glyphs.py` (Pillow con FreeType); la
salida es idéntica byte a byte al repetirla.

Instancias estáticas en lugar de la fuente variable, a propósito: iced resuelve una tipografía
por familia y peso, y una fuente variable entregada completa es una sola tipografía con su
peso por defecto — cada etiqueta saldría en 400 y las distinciones 500/600 del diseño
desaparecerían en silencio.

Para regenerar las estáticas de Instrument Sans:

```
fonttools varLib.instancer InstrumentSans[wdth,wght].ttf wght=<400|500|600> wdth=100 \
    --output InstrumentSans-<Regular|Medium|SemiBold>.ttf
```

luego establece los name IDs 2/4/6 con la subfamilia — el instanciador deja los tres
diciendo "Regular", lo cual es cosmético pero confunde a quien inspeccione el archivo.
