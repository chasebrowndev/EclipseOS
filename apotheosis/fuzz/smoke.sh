#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# CI fuzz smoke (T-18 §11, T-25 §8): every target for SECS seconds (default 60) on this
# workspace's pinned nightly. Needs cargo-fuzz. Usage: fuzz/smoke.sh [SECS] [TARGET...]
set -euo pipefail
cd "$(dirname "$0")"
secs=${1:-60}
shift || true
targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
  mapfile -t targets < <(cargo fuzz list)
fi
# New units go to a scratch dir (libFuzzer writes into its first corpus argument), so a
# smoke run never edits the in-tree seeds.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
for t in "${targets[@]}"; do
  echo "fuzz smoke: $t (${secs}s)"
  mkdir -p "$scratch/$t"
  cargo fuzz run "$t" "$scratch/$t" "corpus/$t" -- -max_total_time="$secs" -rss_limit_mb=4096
done
