/* SPDX-License-Identifier: AGPL-3.0-only
 *
 * Offline generator for the annotation HUD's JetBrains Mono atlases
 * (ADR 0071). The compositor has no font engine and must not grow one at
 * runtime (ADR 0009), so the face is rasterised here, once, and the 8-bit
 * coverage is checked in beside this file and compiled into
 * ec-abyss-render with `include_bytes!` (src/hud_font.rs).
 *
 * Build and run from this directory:
 *
 *     cc -O2 -o gen_atlas gen_atlas.c $(pkg-config --cflags --libs freetype2)
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
 *   8..    419 cells, in this order:
 *          a. printable ASCII 0x20..=0x7e in order (95 cells, indices 0..94)
 *          b. U+00D7 (multiplication sign, the selector's `412 × 128`) and
 *             U+00B7 (middle dot, the selector hint's separator)
 *             (indices 95, 96)
 *          c. the EXTENDED table below, ascending by codepoint (indices
 *             97..418). The annotation HUD draws these; the trusted UI never
 *             does. `hud_font.rs` carries the same list and maps a codepoint
 *             to its cell by binary search, so the two must stay in step.
 *   Each cell is cell_w * cell_h coverage bytes, row-major, top row first.
 *   Any codepoint missing from a face is a hard error here: nothing is
 *   substituted from another font.
 *
 * Glyphs are fixed-pitch: the true advance (0.6 em) is rounded to a whole
 * pixel so a line is a plain blit loop, and each outline is shifted by half
 * the rounding so the ink stays centred in its box. Light hinting (vertical
 * only) keeps baselines and x-height crisp without distorting the shapes.
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
/* Non-ASCII cells after the 97 base cells, ascending. Latin-1 Supplement
 * U+00A0..U+00FF (less U+00AD soft hyphen, an invisible format character,
 * and the two base cells U+00B7 and U+00D7), Latin Extended-A (less
 * U+0132/U+0133, which JetBrains Mono lacks), Greek (U+03A2 skipped; U+03C2
 * is inside the lowercase run), selected superscripts and subscripts (less
 * the unassigned U+2072/U+2073 and U+2071, which the face lacks), arrows,
 * the mathematical operators the HUD's maths needs, and general punctuation.
 * Every entry was checked against both faces' cmap. */
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
    0x2282, 0x2283,
};
#define NEXTRA (sizeof EXTENDED / sizeof EXTENDED[0])
#define BASE (LAST - FIRST + 1 + 2)
#define COUNT (BASE + NEXTRA)

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
    FT_Pos shift = (FT_Pos)lround((adv - true_adv) / 2.0 * 64.0);

    unsigned char *cells = calloc((size_t)COUNT * cw * ch, 1);
    for (size_t i = 0; i < COUNT; i++) {
        FT_ULong c = i <= LAST - FIRST ? FIRST + i
                   : i < BASE          ? (i == LAST - FIRST + 1 ? 0x00d7 : 0x00b7)
                                       : EXTENDED[i - BASE];
        if (!FT_Get_Char_Index(face, c)) {
            fprintf(stderr, "%s has no glyph for U+%04lX\n", f->ttf, c);
            return 1;
        }
        if (FT_Load_Char(face, c, FT_LOAD_NO_BITMAP | FT_LOAD_TARGET_LIGHT)) {
            fprintf(stderr, "no glyph %04lx\n", c);
            return 1;
        }
        FT_Outline *o = &face->glyph->outline;
        if (f->embolden > 0.0) {
            FT_Pos e = (FT_Pos)lround(f->embolden * scale * 64.0);
            FT_Outline_EmboldenXY(o, e, 0);
            FT_Outline_Translate(o, -e / 2, 0);
        }
        FT_Outline_Translate(o, shift, 0);
        if (FT_Render_Glyph(face->glyph, FT_RENDER_MODE_LIGHT)) {
            fprintf(stderr, "render %04lx failed\n", c);
            return 1;
        }
        FT_Bitmap *b = &face->glyph->bitmap;
        int ox = margin + face->glyph->bitmap_left;
        int oy = asc - face->glyph->bitmap_top;
        unsigned char *cell = cells + i * cw * ch;
        for (unsigned y = 0; y < b->rows; y++) {
            for (unsigned x = 0; x < b->width; x++) {
                int tx = ox + (int)x, ty = oy + (int)y;
                if (tx < 0 || ty < 0 || tx >= cw || ty >= ch) {
                    continue; /* clipped overhang; never hit for these glyphs */
                }
                cell[ty * cw + tx] = b->buffer[y * b->pitch + x];
            }
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
