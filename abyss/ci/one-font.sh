#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Onest is the single UI face (docs/STYLE.md, assets/fonts/README.md). Fail when
# a UI crate or stylesheet names a monospace family or a retired face.
#
# Allowed, and so not scanned: the terminal cell grid (apotheosis/), the
# annotation HUD and trusted-UI glyph atlases (abyss/crates/ec-abyss-render,
# trusted_ui, assets/fonts/{hud,trusted}), vendored code, ADRs and the spec
# volumes. ec-ctl's tests use "JetBrains Mono" as an arbitrary config string.
#
# Run from anywhere: abyss/ci/one-font.sh
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"

pat='Font::MONOSPACE|Family::Monospace|font-family:[^;]*monospace|font:[^;]*[ ,]monospace|Instrument ?Sans|JetBrains ?Mono'
hits=$(rg -n --no-heading -e "$pat" \
  shell installer fog oracle-eyes browser packaging/greetd abyss/crates \
  -g '*.rs' -g '*.css' -g '*.toml' -g '*.kdl' \
  -g '!**/target/**' -g '!**/vendor/**' \
  -g '!abyss/crates/ec-abyss-render/**' -g '!**/trusted_ui/**' \
  -g '!abyss/crates/ec-ctl/**' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  echo "::error::a UI crate names a monospace or retired face; use the Onest tokens (docs/STYLE.md)"
  exit 1
fi
echo "one UI font holds"
