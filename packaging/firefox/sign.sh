#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Sign Glass for Firefox through AMO's unlisted channel (ADR 0079) and write the
# result to packaging/firefox/glass.xpi. Owner-run, per version bump; needs
# AMO_JWT_ISSUER and AMO_JWT_SECRET from addons.mozilla.org's API keys page.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
# Keys from the environment, else from a 0600 file holding them in any layout:
# the issuer is the `user:N:N` token, the secret the 64-hex one.
keys=${AMO_KEYS:-$HOME/.config/eclipse/amo-keys}
if [ -z "${AMO_JWT_ISSUER:-}" ] && [ -r "$keys" ]; then
  AMO_JWT_ISSUER=$(grep -oE 'user:[0-9]+:[0-9]+' "$keys" | head -1)
  AMO_JWT_SECRET=$(grep -oE '\b[0-9a-f]{64}\b' "$keys" | head -1)
fi
: "${AMO_JWT_ISSUER:?set AMO_JWT_ISSUER or put the keys in $keys}" "${AMO_JWT_SECRET:?set AMO_JWT_SECRET or put the keys in $keys}"
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
# Keys go through the environment: npx echoes its argv.
WEB_EXT_API_KEY=$AMO_JWT_ISSUER WEB_EXT_API_SECRET=$AMO_JWT_SECRET \
  npx --yes web-ext@10.7.0 sign --channel unlisted \
  -s "$root/browser/firefox-glass" -a "$out"
install -m0644 "$out"/*.xpi "$root/packaging/firefox/glass.xpi"
"$root/packaging/firefox/check-xpi.sh"
