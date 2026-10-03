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
 *   0..4   magic "HUD1"
 *   4      cell width   (pixels)
 *   5      cell height  (pixels)
 *   6      advance      (pixels; cell width minus a margin on each side)
 *   7      margin       (pixels of overhang room left of the advance box)
 *   8..    97 cells: printable ASCII 0x20..=0x7e in order, then
 *          U+00D7 (multiplication sign, the selector's `412 × 128`) and
 *          U+00B7 (middle dot, the selector hint's separator). Each cell is
 *          cell_w * cell_h coverage bytes, row-major, top row first.
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
/* The two non-ASCII glyphs the compositor itself draws; caller text is
 * sanitised to ASCII before it reaches the atlas. */
static const FT_ULong EXTRA[] = {0x00d7, 0x00b7};
#define NEXTRA (sizeof EXTRA / sizeof EXTRA[0])
#define COUNT (LAST - FIRST + 1 + NEXTRA)

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
        FT_ULong c = i <= LAST - FIRST ? FIRST + i : EXTRA[i - (LAST - FIRST + 1)];
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
    unsigned char head[8] = {'H', 'U', 'D', '1', (unsigned char)cw, (unsigned char)ch,
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
