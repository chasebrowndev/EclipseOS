#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Serve the pacman repo on the tailnet interface only (D-02 reduced). Binding
# to the tailnet address rather than 0.0.0.0 is the whole privacy story: the
# repo is reachable from machines on the tailnet and from nowhere else.
set -euo pipefail
ROOT="${ECLIPSEOS_REPO_ROOT:-$HOME/.local/share/eclipseos/repo}"
BIND="$(tailscale ip -4)"
echo "==> http://$BIND:8088/  (also http://$(hostname -s):8088/ via MagicDNS)"
exec python3 -m http.server 8088 --bind "$BIND" --directory "$ROOT"
