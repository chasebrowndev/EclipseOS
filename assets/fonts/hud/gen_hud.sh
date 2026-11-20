#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Regenerate the annotation HUD's atlases (title/body/readout, 1x and 2x) from
# the vendored Onest statics, JetBrains Mono filling the codepoints Onest lacks
# (Greek, maths, arrows). Usage: sh assets/fonts/hud/gen_hud.sh
set -e
here=$(cd "$(dirname "$0")" && pwd)
fonts=$here/..
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cc -O2 -o "$tmp/gen_atlas" "$here/gen_atlas.c" $(pkg-config --cflags --libs freetype2) -lm
gen() { # name font fallback px
    for s in 1 2; do
        "$tmp/gen_atlas" --font "$fonts/$2" --fallback "$fonts/$3" --px "$4" --scale $s \
            --charset "$here/charset.txt" --out "$here/$1-${s}x.a8"
    done
}
gen title Onest-SemiBold.ttf JetBrainsMono-Medium.ttf 13
gen body Onest-Regular.ttf JetBrainsMono-Regular.ttf 12.5
gen readout Onest-Medium.ttf JetBrainsMono-Medium.ttf 11
