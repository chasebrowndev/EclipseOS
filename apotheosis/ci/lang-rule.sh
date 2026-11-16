#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# T-24 §3 / ADR 0110 — the Apotheosis language rule, checked mechanically.
#  1. No C-family source anywhere in the workspace (vendored forks included).
#  2. No crate in the shipped dependency graph compiles C or runs bindgen at build time,
#     except `cc` reached only through rav1d (nasm assembly, `av1-asm`, helper only), and
#     through wayland-backend, whose build script compiles C only with its `log` feature
#     (checked below to be off).
#  3. (with a target dir) every shipped binary links only allow-listed libraries (T-00 §3.2).
# Usage: ci/lang-rule.sh [TARGET_PROFILE_DIR]   — run from apotheosis/.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0

# 1. Source files.
bad=$(find . \( -path ./target -o -path ./fuzz/corpus -o -path ./fuzz/target -o -path '*/tests/fixtures' \) -prune -o \
  -type f \( -name '*.c' -o -name '*.h' -o -name '*.cc' -o -name '*.cpp' -o -name '*.cxx' \
  -o -name '*.hpp' -o -name '*.m' -o -name '*.mm' -o -name '*.S' -o -name '*.s' -o -name '*.asm' \) -print)
if [[ -n "$bad" ]]; then echo "lang-rule: C-family source in the workspace:"; echo "$bad"; fail=1; fi

# 2. Build-time C. Shipped graph = normal + build edges of the shipped crates, all features.
for tool in cc cmake bindgen nasm-rs; do
  if out=$(cargo tree --workspace --all-features -e normal,build -i "$tool" --prefix none 2>/dev/null); then
    [[ -z "$out" ]] && continue
    if [[ "$tool" == cc || "$tool" == nasm-rs ]]; then
      # allowed only when every crate that pulls it in is rav1d
      parents=$(cargo tree --workspace --all-features -e normal,build -i "$tool" --depth 1 --prefix none \
        | tail -n +2 | awk '{print $1}' | sort -u | grep -v -e '^rav1d$' -e '^wayland-backend$' || true)
      if [[ -n "$parents" ]]; then echo "lang-rule: '$tool' reached through: $parents"; fail=1; fi
    else
      echo "lang-rule: '$tool' is in the build graph:"; echo "$out" | head -20; fail=1
    fi
  fi
done

# wayland-backend compiles its C log shim only with feature `log`.
if cargo tree --workspace --all-features -e features -i wayland-backend --prefix none 2>/dev/null \
    | grep -q 'wayland-backend feature "log"'; then
  echo "lang-rule: wayland-backend feature \"log\" is enabled (compiles C)"; fail=1
fi

# 3. Linked libraries of built binaries.
if [[ $# -ge 1 ]]; then
  allow='^(libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|ld-linux-x86-64\.so\.2|ld-linux-aarch64\.so\.1|libwayland-client\.so\.0|libxkbcommon\.so\.0|libva\.so\.2|libva-drm\.so\.2)$'
  for b in apotheosis apoth apoth-media-helper; do
    f="$1/$b"; [[ -x "$f" ]] || continue
    while read -r lib; do
      [[ "$lib" =~ $allow ]] || { echo "lang-rule: $b links $lib (not in T-00 §3.2)"; fail=1; }
    done < <(readelf -d "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  done
fi

if [[ $fail -eq 0 ]]; then echo "lang-rule: ok"; fi
exit $fail
