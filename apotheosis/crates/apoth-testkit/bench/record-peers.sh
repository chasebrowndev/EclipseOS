#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Peer-baseline recorder (T-23 §1, M17). Treats peers as black boxes: runs their CLIs and
# stores wall-clock results as data. It never reads peer source.
#
# usage: record-peers.sh <outdir> [bytes]   (default 64 MiB; run on the reference machine)
# Needs `apoth-bench` on PATH or $APOTH_BENCH. A peer that is not installed is skipped.
# Method: each peer runs `sh -c 'cat corpus; printf <done>'` and we time spawn -> exit,
# minus the same peer running an empty command (start-up/teardown), median of 10, serial.
set -euo pipefail
out=${1:?outdir}; bytes=${2:-67108864}; runs=${RUNS:-10}
bench=${APOTH_BENCH:-apoth-bench}
mkdir -p "$out"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

declare -A cmd=(
  [foot]="foot -e"
  [kitty]="kitty -e"
  [alacritty]="alacritty -e"
  [wezterm]="wezterm start --always-new-process --"
  [ghostty]="ghostty -e"
)
now_ns() { date +%s%N; }
median() { sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
ver() { "$1" --version 2>/dev/null | head -n1 || echo unknown; }

for c in cat-large ls-r vim-scroll csi-redraw dense unicode; do
  "$bench" gen "$c" "$bytes" > "$tmp/$c.bin"
  echo "$c $("$bench" hash "$c" "$bytes")" >> "$out/corpora.txt"
done

for peer in "${!cmd[@]}"; do
  command -v "$peer" >/dev/null || { echo "skip $peer (not installed)"; continue; }
  json="$out/peer-$peer.json"
  { echo "{"; echo "  \"peer\": \"$peer\", \"version\": \"$(ver "$peer")\","
    echo "  \"when\": \"$(date -u +%FT%TZ)\", \"bytes\": $bytes, \"results_ms\": {"; } > "$json"
  base_samples=""
  for _ in $(seq "$runs"); do
    s=$(now_ns); ${cmd[$peer]} sh -c true; e=$(now_ns); base_samples+="$(( (e-s)/1000000 ))"$'\n'
  done
  base=$(printf '%s' "$base_samples" | median)
  first=1
  for c in cat-large ls-r vim-scroll csi-redraw dense unicode; do
    samples=""
    for _ in $(seq "$runs"); do
      s=$(now_ns); ${cmd[$peer]} sh -c "cat '$tmp/$c.bin'"; e=$(now_ns)
      samples+="$(( (e-s)/1000000 ))"$'\n'
    done
    med=$(printf '%s' "$samples" | median)
    [ $first = 1 ] || echo "," >> "$json"; first=0
    printf '    "%s": %s' "$c" "$(( med > base ? med - base : 0 ))" >> "$json"
  done
  printf '\n  },\n  "startup_ms": %s\n}\n' "$base" >> "$json"
  echo "recorded $peer -> $json"
done
