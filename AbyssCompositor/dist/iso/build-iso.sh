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
# Whose repo to bake. SUDO_USER is unset under systemd-run and in a root
# shell, so fall back to the owner of this checkout rather than to root's home.
_owner="${SUDO_USER:-$(stat -c %U "${BASH_SOURCE[0]}")}"
REPO_ROOT="${ECLIPSEOS_REPO_ROOT:-$(getent passwd "$_owner" | cut -d: -f6)/.local/share/eclipseos/repo}"
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

# The database has to match what was actually copied, so rebuild it here
# rather than copying the one that indexes the debug package too.
#
# It has to be *signed*, not just rebuilt. The drop-in the installed system
# ships is `DatabaseRequired`, and pacstrap leaves this database behind in
# /var/lib/pacman/sync -- so an unsigned one here means the first `pacman -S`
# the owner ever types on the new machine dies with "missing required
# signature". Deleting it instead is not the fix: pacman fails a transaction
# outright when a configured repo has no database at all, which strands a
# laptop that is not yet on the tailnet.
#
# This runs as root, and the packaging key is passphraseless in the owner's
# keyring, so point GPG at it for this one command rather than expecting root
# to have its own.
_gnupg="$(getent passwd "$_owner" | cut -d: -f6)/.local/share/eclipseos/gnupg"
[[ -d "$_gnupg" ]] || {
  echo "no packaging keyring at $_gnupg -- run dist/repo/build-repo.sh first" >&2
  exit 1
}
GNUPGHOME="$_gnupg" repo-add -q --sign --key "$KEYID" \
  "$baked/eclipseos.db.tar.zst" "$baked"/*.pkg.tar.zst

# The medium carries the key so the installer can verify what it pacstraps.
cp -a "$KEYFILE" "$here/airootfs/root/eclipseos-packaging.asc"

# A `build-repo.sh local` rebuild keeps the same pkgver-pkgrel, so pacman's
# cache holds a file with the right name and the wrong checksum. Drop the
# EclipseOS packages from it; they are one repo build away at any time.
rm -f /var/cache/pacman/pkg/eclipseos-*.pkg.tar.zst{,.sig}

echo "==> mkarchiso"
rm -rf "$WORK"
mkarchiso -v -w "$WORK" -o "$OUT" "$here"

echo
echo "==> iso in $OUT"
