#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# D-03: bake the signed EclipseOS packages into the medium, then run mkarchiso.
#
#   sudo ./build-iso.sh
#
# The profile's [eclipseos] repo is a file:// path on THIS machine, so mkarchiso
# only runs here. The ISO it produces is self-contained and installs anywhere:
# airootfs/root/eclipseos-repo is the copy the installer pacstraps from.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${ECLIPSEOS_REPO_ROOT:-${SUDO_USER:+/home/$SUDO_USER}/.local/share/eclipseos/repo}"
WORK="${ECLIPSEOS_ISO_WORK:-/var/tmp/eclipseos-work}"
OUT="${ECLIPSEOS_ISO_OUT:-/var/tmp/eclipseos-out}"

[[ $EUID -eq 0 ]] || { echo "mkarchiso needs root" >&2; exit 1; }
[[ -d "$REPO_ROOT/x86_64" ]] || {
  echo "no built repo at $REPO_ROOT/x86_64 -- run dist/repo/build-repo.sh first" >&2
  exit 1
}

echo "==> baking $REPO_ROOT/x86_64 into the medium"
baked="$here/airootfs/root/eclipseos-repo"
rm -rf "$baked"
mkdir -p "$baked"
# Everything but the debug package: it is 440M of split symbols against 49M of
# actual system, and nothing on the install path pulls it. Fetch it from the
# tailnet repo on the rare occasion you want a backtrace.
for p in "$REPO_ROOT"/x86_64/*.pkg.tar.zst; do
  [[ "$(basename "$p")" == *-debug-* ]] && continue
  cp -a "$p" "$p.sig" "$baked/" 2>/dev/null || cp -a "$p" "$baked/"
done
# The database has to match what was actually copied, so rebuild it here
# rather than copying the one that indexes the debug package too.
repo-add -q "$baked/eclipseos.db.tar.zst" "$baked"/*.pkg.tar.zst

# TrustAll only relaxes the web-of-trust requirement; a key pacman has never
# seen still fails verification outright. The build host has to actually hold
# the packaging key to read a repo-add --sign database.
KEYFILE="$REPO_ROOT/eclipseos-packaging.asc"
[[ -f "$KEYFILE" ]] || { echo "no packaging key at $KEYFILE" >&2; exit 1; }
KEYID="$(gpg --show-keys --with-colons "$KEYFILE" | awk -F: '/^fpr:/{print $10; exit}')"
if ! pacman-key --list-keys "$KEYID" >/dev/null 2>&1; then
  echo "==> importing packaging key $KEYID into the build host keyring"
  pacman-key --add "$KEYFILE"
  pacman-key --lsign-key "$KEYID"
fi

# The medium carries the key so the installer can verify what it pacstraps.
cp -a "$KEYFILE" "$here/airootfs/root/eclipseos-packaging.asc"

echo "==> mkarchiso"
rm -rf "$WORK"
mkarchiso -v -w "$WORK" -o "$OUT" "$here"

echo
echo "==> iso in $OUT"
