#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# The committed signed XPI must hold exactly the reviewed source (ADR 0079):
# its files, minus AMO's META-INF signature, equal browser/firefox-glass/.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
xpi="$root/packaging/firefox/glass.xpi"
[ -f "$xpi" ] || { echo "::error::$xpi missing; run packaging/firefox/sign.sh"; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unzip -q "$xpi" -d "$tmp"
rm -rf "$tmp/META-INF"
diff -r "$tmp" "$root/browser/firefox-glass" \
  || { echo "::error::glass.xpi differs from browser/firefox-glass; re-sign"; exit 1; }
echo "glass.xpi matches source"
