#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Sign Glass for Firefox through AMO's unlisted channel (ADR 0079) and write the
# result to packaging/firefox/glass.xpi. Owner-run, per version bump; needs
# AMO_JWT_ISSUER and AMO_JWT_SECRET from addons.mozilla.org's API keys page.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
: "${AMO_JWT_ISSUER:?set AMO_JWT_ISSUER}" "${AMO_JWT_SECRET:?set AMO_JWT_SECRET}"
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
npx --yes web-ext@8.3.0 sign --channel unlisted \
  -s "$root/browser/firefox-glass" -a "$out" \
  --api-key "$AMO_JWT_ISSUER" --api-secret "$AMO_JWT_SECRET"
install -m0644 "$out"/*.xpi "$root/packaging/firefox/glass.xpi"
"$root/packaging/firefox/check-xpi.sh"
