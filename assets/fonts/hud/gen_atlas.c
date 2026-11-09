/* SPDX-License-Identifier: AGPL-3.0-only
 *
 * Offline generator for the annotation HUD's JetBrains Mono atlases
 * (ADR 0071, amended by ADR 0083). The compositor has no font engine and must
 * not grow one at runtime (ADR 0009), so the face is rasterised here, once,
 * and the 8-bit coverage is checked in beside this file and compiled into
 * ec-abyss-render with `include_bytes!` (src/hud_font.rs).
 *
 * Build and run from this directory:
 *
 *     cc -O2 -o gen_atlas gen_atlas.c $(pkg-config --cflags --libs freetype2) -lm
 *     ./gen_atlas ..
 *
 * The argument is the directory holding the JetBrains Mono TTFs. Output is
 * one `<face>-<scale>x.a8` per face and scale, overwritten in place.
 *
 * File format (all u8):
 *   0..4   magic "HUD2"
 *   4      cell width   (pixels)
 *   5      cell height  (pixels)
 *   6      advance      (pixels; cell width minus a margin on each side)
 *   7      margin       (pixels of overhang room left of the advance box)
 *   8..    435 cells, in this order:
 *          a. printable ASCII 0x20..=0x7e in order (95 cells, indices 0..94)
 *          b. U+00D7 (multiplication sign, the selector's `412 × 128`) and
 *             U+00B7 (middle dot, the selector hint's separator)
 *             (indices 95, 96)
 *          c. the EXTENDED table below, ascending by codepoint (indices
 *             97..434). The annotation HUD draws these; the trusted UI never
 *             does. The last eleven are the private-use composed cells of
 *             COMPOSED below. `hud_font.rs` carries the same lists and maps a
 *             codepoint to its cell by binary search, so the two must stay in
 *             step.
 *   Each cell is cell_w * cell_h coverage bytes, row-major, top row first.
 *   Any codepoint missing from a face is a hard error here: nothing is
 *   substituted from another font.
 *
 * Glyphs are fixed-pitch: the true advance (0.6 em) is rounded to a whole
 * pixel so a line is a plain blit loop, and each outline is shifted by half
 * the rounding so the ink stays centred in its box. Light hinting (vertical
 * only) keeps baselines and x-height crisp without distorting the shapes.
 *
 * Composed cells (COMPOSED): a base letter with a combining mark over it,
 * rasterised as one cell. The base is drawn as any cell is. The mark comes
 * from the same face: JetBrains Mono's marks are zero-advance overlays with
 * their ink to the left of the pen, so the mark is moved to centre on the
 * advance box, then lifted if it would sit on a tall base (ascender or
 * Greek top). It is composited with max() so the two inks never cancel. A
 * mark that would leave the cell is a hard error, not a silent clip.
 */
#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_OUTLINE_H
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct face {
    const char *name; /* output stem */
    const char *ttf;  /* source file */
    double px;        /* size at 1x, in pixels per em */
    double embolden;  /* extra stem width at 1x, in pixels (0 = none) */
};

/* Sizes from the design audit: title ~13 bold, body ~12.5, readout 11.
 * There is no Bold static vendored; Medium plus a fraction of a pixel of
 * outline emboldening lands between SemiBold and Bold, which is what the
 * title needs to read as a heading without double-striking at draw time. */
static const struct face FACES[] = {
    {"title", "JetBrainsMono-Medium.ttf", 13.0, 0.35},
    {"body", "JetBrainsMono-Regular.ttf", 12.5, 0.0},
    {"readout", "JetBrainsMono-Medium.ttf", 11.0, 0.0},
};

#define FIRST 0x20
#define LAST 0x7e
#define PUA_FIRST 0xe000
/* Non-ASCII cells after the 97 base cells, ascending. Latin-1 Supplement
 * U+00A0..U+00FF (less U+00AD soft hyphen, an invisible format character,
 * and the two base cells U+00B7 and U+00D7), Latin Extended-A (less
 * U+0132/U+0133, which JetBrains Mono lacks), Greek (U+03A2 skipped; U+03C2
 * is inside the lowercase run), selected superscripts and subscripts (less
 * the unassigned U+2072/U+2073 and U+2071, which the face lacks), arrows,
 * the mathematical operators the HUD's maths needs, general punctuation, and
 * a few title symbols (circles, check and cross marks, and the six-pointed
 * star U+2736, one frame of Claude Code's title spinner; its sibling frames
 * U+2722/2733/273B/273D are not in the face).
 * Every entry was checked against both faces' cmap. The private-use tail
 * (U+E000..) is the composed cells; its order is COMPOSED's order. */
static const FT_ULong EXTENDED[] = {
    0x00a0, 0x00a1, 0x00a2, 0x00a3, 0x00a4, 0x00a5, 0x00a6, 0x00a7,
    0x00a8, 0x00a9, 0x00aa, 0x00ab, 0x00ac, 0x00ae, 0x00af, 0x00b0,
    0x00b1, 0x00b2, 0x00b3, 0x00b4, 0x00b5, 0x00b6, 0x00b8, 0x00b9,
    0x00ba, 0x00bb, 0x00bc, 0x00bd, 0x00be, 0x00bf, 0x00c0, 0x00c1,
    0x00c2, 0x00c3, 0x00c4, 0x00c5, 0x00c6, 0x00c7, 0x00c8, 0x00c9,
    0x00ca, 0x00cb, 0x00cc, 0x00cd, 0x00ce, 0x00cf, 0x00d0, 0x00d1,
    0x00d2, 0x00d3, 0x00d4, 0x00d5, 0x00d6, 0x00d8, 0x00d9, 0x00da,
    0x00db, 0x00dc, 0x00dd, 0x00de, 0x00df, 0x00e0, 0x00e1, 0x00e2,
    0x00e3, 0x00e4, 0x00e5, 0x00e6, 0x00e7, 0x00e8, 0x00e9, 0x00ea,
    0x00eb, 0x00ec, 0x00ed, 0x00ee, 0x00ef, 0x00f0, 0x00f1, 0x00f2,
    0x00f3, 0x00f4, 0x00f5, 0x00f6, 0x00f7, 0x00f8, 0x00f9, 0x00fa,
    0x00fb, 0x00fc, 0x00fd, 0x00fe, 0x00ff, 0x0100, 0x0101, 0x0102,
    0x0103, 0x0104, 0x0105, 0x0106, 0x0107, 0x0108, 0x0109, 0x010a,
    0x010b, 0x010c, 0x010d, 0x010e, 0x010f, 0x0110, 0x0111, 0x0112,
    0x0113, 0x0114, 0x0115, 0x0116, 0x0117, 0x0118, 0x0119, 0x011a,
    0x011b, 0x011c, 0x011d, 0x011e, 0x011f, 0x0120, 0x0121, 0x0122,
    0x0123, 0x0124, 0x0125, 0x0126, 0x0127, 0x0128, 0x0129, 0x012a,
    0x012b, 0x012c, 0x012d, 0x012e, 0x012f, 0x0130, 0x0131, 0x0134,
    0x0135, 0x0136, 0x0137, 0x0138, 0x0139, 0x013a, 0x013b, 0x013c,
    0x013d, 0x013e, 0x013f, 0x0140, 0x0141, 0x0142, 0x0143, 0x0144,
    0x0145, 0x0146, 0x0147, 0x0148, 0x0149, 0x014a, 0x014b, 0x014c,
    0x014d, 0x014e, 0x014f, 0x0150, 0x0151, 0x0152, 0x0153, 0x0154,
    0x0155, 0x0156, 0x0157, 0x0158, 0x0159, 0x015a, 0x015b, 0x015c,
    0x015d, 0x015e, 0x015f, 0x0160, 0x0161, 0x0162, 0x0163, 0x0164,
    0x0165, 0x0166, 0x0167, 0x0168, 0x0169, 0x016a, 0x016b, 0x016c,
    0x016d, 0x016e, 0x016f, 0x0170, 0x0171, 0x0172, 0x0173, 0x0174,
    0x0175, 0x0176, 0x0177, 0x0178, 0x0179, 0x017a, 0x017b, 0x017c,
    0x017d, 0x017e, 0x017f, 0x0391, 0x0392, 0x0393, 0x0394, 0x0395,
    0x0396, 0x0397, 0x0398, 0x0399, 0x039a, 0x039b, 0x039c, 0x039d,
    0x039e, 0x039f, 0x03a0, 0x03a1, 0x03a3, 0x03a4, 0x03a5, 0x03a6,
    0x03a7, 0x03a8, 0x03a9, 0x03b1, 0x03b2, 0x03b3, 0x03b4, 0x03b5,
    0x03b6, 0x03b7, 0x03b8, 0x03b9, 0x03ba, 0x03bb, 0x03bc, 0x03bd,
    0x03be, 0x03bf, 0x03c0, 0x03c1, 0x03c2, 0x03c3, 0x03c4, 0x03c5,
    0x03c6, 0x03c7, 0x03c8, 0x03c9, 0x2013, 0x2014, 0x2018, 0x2019,
    0x201c, 0x201d, 0x2022, 0x2026, 0x2070, 0x2074, 0x2075, 0x2076,
    0x2077, 0x2078, 0x2079, 0x2080, 0x2081, 0x2082, 0x2083, 0x2084,
    0x2085, 0x2086, 0x2087, 0x2088, 0x2089, 0x20ac, 0x2190, 0x2191,
    0x2192, 0x2193, 0x2194, 0x2195, 0x2200, 0x2202, 0x2203, 0x2206,
    0x2207, 0x2208, 0x2211, 0x2212, 0x221a, 0x221e, 0x2227, 0x2228,
    0x2229, 0x222a, 0x222b, 0x2248, 0x2260, 0x2261, 0x2264, 0x2265,
    0x2282, 0x2283, 0x25cb, 0x25cf, 0x2713, 0x2717, 0x2736,
    /* Composed cells, one per COMPOSED entry, in its order. */
    0xe000, 0xe001, 0xe002, 0xe003, 0xe004, 0xe005, 0xe006, 0xe007,
    0xe008, 0xe009, 0xe00a,
};
#define NEXTRA (sizeof EXTENDED / sizeof EXTENDED[0])
#define BASE (LAST - FIRST + 1 + 2)
#define COUNT (BASE + NEXTRA)

/* A base letter and a combining mark, drawn as one cell at `pua`. text.rs
 * folds the two-codepoint sequence into `pua`; hud_font.rs carries the same
 * triples as COMPOSED. y with circumflex (U+0177) and y with macron (U+0233)
 * are not generated: U+0177 is already a precomposed cell and U+0233 is not
 * in the atlas, so a y mark is stripped (ADR 0083 amendment). */
struct composed {
    FT_ULong base, mark, pua;
};
static const struct composed COMPOSED[] = {
    {0x70, 0x302, 0xe000},  /* p with circumflex */
    {0x78, 0x302, 0xe001},  /* x with circumflex */
    {0x3b2, 0x302, 0xe002}, /* beta with circumflex */
    {0x3c3, 0x302, 0xe003}, /* sigma with circumflex */
    {0x3b8, 0x302, 0xe004}, /* theta with circumflex */
    {0x3bc, 0x302, 0xe005}, /* mu with circumflex */
    {0x3bb, 0x302, 0xe006}, /* lambda with circumflex */
    {0x3c1, 0x302, 0xe007}, /* rho with circumflex */
    {0x78, 0x304, 0xe008},  /* x with macron */
    {0x70, 0x304, 0xe009},  /* p with macron */
    {0x3bc, 0x304, 0xe00a}, /* mu with macron */
};
#define NCOMPOSED (sizeof COMPOSED / sizeof COMPOSED[0])

/* Per-atlas geometry, shared by every cell of one file. */
struct geom {
    int cw, ch, asc, margin, scale, adv;
    FT_Pos shift;     /* centring shift, 26.6 */
    FT_Pos embolden;  /* stem emboldening, 26.6; 0 = none */
};

static const struct composed *composed_for(FT_ULong pua) {
    for (size_t k = 0; k < NCOMPOSED; k++) {
        if (COMPOSED[k].pua == pua) {
            return &COMPOSED[k];
        }
    }
    return NULL;
}

/* Load glyph `c` and style it as the face's cells are: emboldening, then the
 * centring shift. Leaves the outline in face->glyph. */
static int load_styled(FT_Face face, const char *ttf, FT_ULong c, const struct face *f,
                       const struct geom *g) {
    if (!FT_Get_Char_Index(face, c)) {
        fprintf(stderr, "%s has no glyph for U+%04lX\n", ttf, c);
        return 1;
    }
    if (FT_Load_Char(face, c, FT_LOAD_NO_BITMAP | FT_LOAD_TARGET_LIGHT)) {
        fprintf(stderr, "no glyph %04lx\n", c);
        return 1;
    }
    FT_Outline *o = &face->glyph->outline;
    if (f->embolden > 0.0) {
        FT_Outline_EmboldenXY(o, g->embolden, 0);
        FT_Outline_Translate(o, -g->embolden / 2, 0);
    }
    FT_Outline_Translate(o, g->shift, 0);
    return 0;
}

/* Rasterise the loaded outline onto the cell at its baseline. A mark is
 * composited with max(); anything it would leave the cell is an error. */
static int blit(FT_Face face, const struct geom *g, unsigned char *cell, int mark,
                FT_ULong c) {
    if (FT_Render_Glyph(face->glyph, FT_RENDER_MODE_LIGHT)) {
        fprintf(stderr, "render %04lx failed\n", c);
        return 1;
    }
    FT_Bitmap *b = &face->glyph->bitmap;
    int ox = g->margin + face->glyph->bitmap_left;
    int oy = g->asc - face->glyph->bitmap_top;
    for (unsigned y = 0; y < b->rows; y++) {
        for (unsigned x = 0; x < b->width; x++) {
            unsigned char v = b->buffer[y * b->pitch + x];
            int tx = ox + (int)x, ty = oy + (int)y;
            if (tx < 0 || ty < 0 || tx >= g->cw || ty >= g->ch) {
                if (mark && v) {
                    fprintf(stderr, "mark U+%04lX clipped by its cell\n", c);
                    return 1;
                }
                continue; /* clipped overhang; never hit for plain glyphs */
            }
            unsigned char *p = &cell[ty * g->cw + tx];
            if (!mark || v > *p) {
                *p = v;
            }
        }
    }
    return 0;
}

/* A composed cell: the base as any cell, then the mark moved onto the
 * advance box's centre line and lifted clear of a tall base. */
static int draw_composed(FT_Face face, const char *ttf, const struct face *f,
                         const struct geom *g, const struct composed *k, unsigned char *cell) {
    if (load_styled(face, ttf, k->base, f, g) || blit(face, g, cell, 0, k->base)) {
        return 1;
    }
    FT_BBox base_box;
    FT_Outline_Get_CBox(&face->glyph->outline, &base_box);
    if (load_styled(face, ttf, k->mark, f, g)) {
        return 1;
    }
    FT_Outline *o = &face->glyph->outline;
    FT_BBox mark_box;
    FT_Outline_Get_CBox(o, &mark_box);
    /* The mark has zero advance and its ink sits left of the pen, so its own
     * position says nothing about the cell. Centre it on the advance box,
     * whose centre is adv / 2 px from the pen: adv * 32 in 26.6. */
    FT_Pos dx = (FT_Pos)g->adv * 32 - (mark_box.xMin + mark_box.xMax) / 2;
    FT_Pos gap = (FT_Pos)g->scale * 64; /* one pixel at this scale */
    FT_Pos dy = 0;
    if (mark_box.yMin < base_box.yMax + gap) {
        dy = base_box.yMax + gap - mark_box.yMin;
    }
    FT_Outline_Translate(o, dx, dy);
    return blit(face, g, cell, 1, k->mark);
}

static int gen(FT_Library lib, const char *dir, const struct face *f, int scale) {
    char path[1024];
    snprintf(path, sizeof path, "%s/%s", dir, f->ttf);
    FT_Face face;
    if (FT_New_Face(lib, path, 0, &face)) {
        fprintf(stderr, "cannot open %s\n", path);
        return 1;
    }
    double px = f->px * scale;
    /* 26.6 fixed point; FT_Set_Char_Size takes 1/64 pt at 72 dpi = px. */
    FT_Set_Char_Size(face, 0, (FT_F26Dot6)lround(px * 64.0), 72, 72);

    double em = face->units_per_EM;
    double true_adv = 600.0 / em * px; /* JetBrains Mono is 600 units wide */
    int adv = (int)lround(true_adv);
    int margin = scale + 1;
    int cw = adv + 2 * margin;
    int asc = (int)ceil(face->ascender / em * px);
    int desc = (int)ceil(-face->descender / em * px);
    int ch = asc + desc;
    if (cw > 255 || ch > 255) {
        fprintf(stderr, "cell too large\n");
        return 1;
    }
    /* Shift the outline so the rounding is split evenly either side. */
    struct geom g = {
        .cw = cw,
        .ch = ch,
        .asc = asc,
        .margin = margin,
        .scale = scale,
        .adv = adv,
        .shift = (FT_Pos)lround((adv - true_adv) / 2.0 * 64.0),
        .embolden = f->embolden > 0.0 ? (FT_Pos)lround(f->embolden * scale * 64.0) : 0,
    };

    unsigned char *cells = calloc((size_t)COUNT * cw * ch, 1);
    for (size_t i = 0; i < COUNT; i++) {
        FT_ULong c = i <= LAST - FIRST ? FIRST + i
                   : i < BASE          ? (i == LAST - FIRST + 1 ? 0x00d7 : 0x00b7)
                                       : EXTENDED[i - BASE];
        unsigned char *cell = cells + i * cw * ch;
        int bad;
        if (c >= PUA_FIRST) {
            const struct composed *k = composed_for(c);
            if (!k) {
                fprintf(stderr, "no composition for U+%04lX\n", c);
                return 1;
            }
            bad = draw_composed(face, f->ttf, f, &g, k, cell);
        } else {
            bad = load_styled(face, f->ttf, c, f, &g) || blit(face, &g, cell, 0, c);
        }
        if (bad) {
            return 1;
        }
    }

    snprintf(path, sizeof path, "%s-%dx.a8", f->name, scale);
    FILE *out = fopen(path, "wb");
    if (!out) {
        fprintf(stderr, "cannot write %s\n", path);
        return 1;
    }
    unsigned char head[8] = {'H', 'U', 'D', '2', (unsigned char)cw, (unsigned char)ch,
                             (unsigned char)adv, (unsigned char)margin};
    fwrite(head, 1, sizeof head, out);
    fwrite(cells, 1, (size_t)COUNT * cw * ch, out);
    fclose(out);
    free(cells);
    printf("%s: cell %dx%d advance %d margin %d ascent %d\n", path, cw, ch, adv, margin, asc);
    FT_Done_Face(face);
    return 0;
}

int main(int argc, char **argv) {
    const char *dir = argc > 1 ? argv[1] : "..";
    FT_Library lib;
    if (FT_Init_FreeType(&lib)) {
        return 1;
    }
    for (size_t i = 0; i < sizeof FACES / sizeof FACES[0]; i++) {
        for (int s = 1; s <= 2; s++) {
            if (gen(lib, dir, &FACES[i], s)) {
                return 1;
            }
        }
    }
    FT_Done_FreeType(lib);
    return 0;
}
