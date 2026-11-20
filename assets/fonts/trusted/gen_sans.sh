#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Rebuild sans-{1,2,3}x.bin + sans-metrics.txt (deterministic). Needs cc + freetype2.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cc -O1 -o "$tmp/gen_sans" "$here/gen_sans.c" $(pkg-config --cflags --libs freetype2) -lm
"$tmp/gen_sans" "$here/.." "$here"
