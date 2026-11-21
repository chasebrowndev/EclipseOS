// SPDX-License-Identifier: AGPL-3.0-only
// Pre-rasterise Onest (OFL) for the trusted prompts: proportional, 3 weights,
// scales 1..3, printable ASCII. See gen_sans.sh. Output format: sans-Nx.bin
// "TSN1" weights count padx cell_h base scale | adv1[3*95] | cells.
#include <ft2build.h>
#include FT_FREETYPE_H
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define PADX 2
#define CELL_H 16
#define BASE 12
#define NG 95
#define PX 13
static const char *FACES[3] = {"Onest-Regular.ttf", "Onest-Medium.ttf", "Onest-SemiBold.ttf"};
static const char *WN[3] = {"Regular", "Medium", "SemiBold"};

int main(int argc, char **argv) {
    if (argc != 3) { fprintf(stderr, "usage: gen_sans <fontdir> <outdir>\n"); return 2; }
    FT_Library lib;
    FT_Face face[3];
    if (FT_Init_FreeType(&lib)) return 1;
    int adv[3][NG];
    char path[512];
    FILE *m;
    snprintf(path, sizeof path, "%s/sans-metrics.txt", argv[2]);
    m = fopen(path, "w");
    fprintf(m, "# Onest, %d px at 1x, unhinted. adv1 = round(advance) in logical px.\n", PX);
    fprintf(m, "# cell = (adv1 + 2*%d)*s wide, %d*s tall, baseline row %d*s.\n", PADX, CELL_H, BASE);
    for (int w = 0; w < 3; w++) {
        snprintf(path, sizeof path, "%s/%s", argv[1], FACES[w]);
        if (FT_New_Face(lib, path, 0, &face[w])) { fprintf(stderr, "no %s\n", path); return 1; }
        FT_Set_Pixel_Sizes(face[w], 0, PX);
        FT_Size_Metrics *sm = &face[w]->size->metrics;
        fprintf(m, "# %s line metrics 1x: ascender %.2f descender %.2f height %.2f\n", WN[w],
                sm->ascender / 64.0, sm->descender / 64.0, sm->height / 64.0);
        for (int g = 0; g < NG; g++) {
            if (FT_Load_Char(face[w], 0x20 + g, FT_LOAD_NO_HINTING | FT_LOAD_NO_BITMAP)) return 1;
            adv[w][g] = (int)lround(face[w]->glyph->linearHoriAdvance / 65536.0);
            if (adv[w][g] < 0 || adv[w][g] > 40) return 1;
        }
    }
    fprintf(m, "\nglyph  Regular Medium SemiBold\n");
    for (int g = 0; g < NG; g++) {
        int c = 0x20 + g;
        fprintf(m, "0x%02X '%c'  %d %d %d\n", c, c, adv[0][g], adv[1][g], adv[2][g]);
    }
    fclose(m);
    int clipped = 0;
    for (int s = 1; s <= 3; s++) {
        snprintf(path, sizeof path, "%s/sans-%dx.bin", argv[2], s);
        FILE *o = fopen(path, "wb");
        unsigned char hdr[10] = {'T', 'S', 'N', '1', 3, NG, PADX, CELL_H, BASE, (unsigned char)s};
        fwrite(hdr, 1, 10, o);
        for (int w = 0; w < 3; w++)
            for (int g = 0; g < NG; g++) fputc(adv[w][g], o);
        for (int w = 0; w < 3; w++) {
            FT_Set_Pixel_Sizes(face[w], 0, PX * s);
            for (int g = 0; g < NG; g++) {
                int cw = (adv[w][g] + 2 * PADX) * s, ch = CELL_H * s;
                unsigned char *cell = calloc((size_t)cw * ch, 1);
                if (FT_Load_Char(face[w], 0x20 + g, FT_LOAD_NO_HINTING | FT_LOAD_NO_BITMAP | FT_LOAD_RENDER)) return 1;
                FT_GlyphSlot sl = face[w]->glyph;
                FT_Bitmap *b = &sl->bitmap;
                int ox = PADX * s + sl->bitmap_left, oy = BASE * s - sl->bitmap_top;
                for (unsigned r = 0; r < b->rows; r++)
                    for (unsigned k = 0; k < b->width; k++) {
                        unsigned char v = b->buffer[r * b->pitch + k];
                        int x = ox + (int)k, y = oy + (int)r;
                        if (x < 0 || x >= cw || y < 0 || y >= ch) {
                            if (v) { clipped++; fprintf(stderr, "CLIP %s '%c' s%d at %d,%d\n", WN[w], 0x20 + g, s, x, y); }
                            continue;
                        }
                        cell[y * cw + x] = v;
                    }
                fwrite(cell, 1, (size_t)cw * ch, o);
                free(cell);
            }
        }
        fclose(o);
    }
    fprintf(stderr, "clipped pixels: %d\n", clipped);
    return clipped ? 3 : 0;
}
