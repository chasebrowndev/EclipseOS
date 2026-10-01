#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# D-02 (reduced): tag -> build -> sign -> repo-add, for a private tailnet repo.
# Audience is the repo owner only; see docs/design/D-02 for what that drops.
#
#   ./build-repo.sh v0.1.0
#
# Signing uses a dedicated packaging key in an isolated keyring. It never
# touches ~/.gnupg.
set -euo pipefail

# `build-repo.sh local` builds from this working tree's HEAD instead of a
# pushed tag. Packaging bugs only surface under mkarchiso, and making every
# one-line fix wait on a tag -- which means a PR, which means the full gate
# and wlcs -- is minutes of CI per character. Iterate with `local`, then build
# the real tag once when it works.
TAG="${1:?usage: build-repo.sh <tag>|local}"
REPO_NAME=eclipseos
REPO_ROOT="${ECLIPSEOS_REPO_ROOT:-$HOME/.local/share/eclipseos/repo}"
export GNUPGHOME="${ECLIPSEOS_GNUPGHOME:-$HOME/.local/share/eclipseos/gnupg}"
KEY_UID="EclipseOS Packaging <packaging@eclipseos.invalid>"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
build_dir="$here/../pkg/eclipseos"
repo_top="$(git -C "$here" rev-parse --show-toplevel)"

# --- packaging key -----------------------------------------------------------
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
if ! gpg --list-secret-keys "$KEY_UID" >/dev/null 2>&1; then
  echo "==> generating the EclipseOS packaging key in $GNUPGHOME"
  gpg --batch --passphrase '' --quick-generate-key "$KEY_UID" ed25519 sign 5y
fi
KEY_ID="$(gpg --list-secret-keys --with-colons "$KEY_UID" | awk -F: '/^fpr:/{print $10; exit}')"
echo "==> signing key $KEY_ID"

# --- build -------------------------------------------------------------------
(
  cd "$build_dir"
  # Only the staging root is disposable. src/ holds the git checkout and its
  # cargo target/ dir; deleting it turns every run into a cold release build
  # (~30 min). makepkg re-fetches the tag into the existing checkout itself.
  rm -rf pkg
  [[ "$TAG" == local ]] ||
    sed -i "s/^pkgver=.*/pkgver=${TAG#v}/" PKGBUILD

  # makepkg will not re-checkout an srcdir that already exists, so a moved or
  # newer tag would silently build the previous one. Reset the existing
  # checkout to the tag ourselves and skip makepkg's extract step; that keeps
  # target/ and makes a repeat build incremental instead of a cold ~30 min.
  extract=()
  if [[ -d src/EclipseOS/.git ]]; then
    if [[ "$TAG" == local ]]; then
      git -C src/EclipseOS fetch --force "$repo_top" HEAD
      git -C src/EclipseOS checkout -f --detach FETCH_HEAD
    else
      git -C src/EclipseOS fetch --tags --force "$repo_top"
      git -C src/EclipseOS checkout -f --detach "refs/tags/$TAG"
    fi
    git -C src/EclipseOS clean -fd -e target
    extract=(--noextract)
  elif [[ "$TAG" == local ]]; then
    echo "local mode needs an existing checkout; run once against a tag first" >&2
    exit 1
  fi
  # check() is `cargo test --workspace --release`, which is most of the runtime
  # and is already a required CI check on every push. Opt back in with
  # ECLIPSEOS_CHECK=1 when packaging a release you have not pushed.
  check_flag=--nocheck
  [[ "${ECLIPSEOS_CHECK:-0}" == 1 ]] && check_flag=--check
  makepkg -sf "${extract[@]}" "$check_flag" --noconfirm --sign --key "$KEY_ID"
)

# --- publish -----------------------------------------------------------------
mkdir -p "$REPO_ROOT/x86_64"
mv -f "$build_dir"/*.pkg.tar.zst "$REPO_ROOT/x86_64/"
mv -f "$build_dir"/*.pkg.tar.zst.sig "$REPO_ROOT/x86_64/" 2>/dev/null || true
# Rebuild the database from what is actually on disk rather than adding into
# the old one: an incremental repo-add keeps entries for packages that have
# since been removed or changed arch, and pacman then asks for a file that is
# not there. (eclipseos-meta went from x86_64 to any and did exactly that.)
rm -f "$REPO_ROOT/x86_64/$REPO_NAME".db* "$REPO_ROOT/x86_64/$REPO_NAME".files*
repo-add --sign --key "$KEY_ID" \
  "$REPO_ROOT/x86_64/$REPO_NAME.db.tar.zst" "$REPO_ROOT/x86_64"/*.pkg.tar.zst

# The public key clients need in order to trust the repo. Served alongside it.
gpg --armor --export "$KEY_ID" >"$REPO_ROOT/eclipseos-packaging.asc"

echo
echo "==> repo at $REPO_ROOT"
echo "    serve it:  $here/serve.sh"
echo "    key id:    $KEY_ID"
