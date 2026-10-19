#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Publish the signed repo that build-repo.sh produced as the assets of the
# rolling `repo-x86_64` GitHub release, which is what installed systems'
# /etc/pacman.d/eclipseos.conf points at (D-02).
#
#   ./publish-github.sh
#
# Release assets cannot be symlinks, so the database is uploaded under the
# names pacman asks for. Assets that are no longer in the repo are deleted, so
# the release mirrors the local repo exactly. The debug package is skipped:
# nothing installs it, and it is most of a gigabyte.
set -euo pipefail

REPO_ROOT="${ECLIPSEOS_REPO_ROOT:-$HOME/.local/share/eclipseos/repo}"
GH_REPO="${ECLIPSEOS_GH_REPO:-chasebrowndev/EclipseOS}"
TAG=repo-x86_64
src="$REPO_ROOT/x86_64"

[[ -f "$src/eclipseos.db.tar.zst" ]] || {
  echo "no built repo at $src -- run build-repo.sh first" >&2
  exit 1
}

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
for f in "$src"/*.pkg.tar.zst "$src"/*.pkg.tar.zst.sig; do
  [[ "$(basename "$f")" == *-debug-* ]] && continue
  cp "$f" "$stage/"
done
for n in db files; do
  cp "$src/eclipseos.$n.tar.zst" "$stage/eclipseos.$n"
  cp "$src/eclipseos.$n.tar.zst.sig" "$stage/eclipseos.$n.sig"
done
cp "$REPO_ROOT/eclipseos-packaging.asc" "$stage/"

if ! gh release view "$TAG" -R "$GH_REPO" >/dev/null 2>&1; then
  gh release create "$TAG" -R "$GH_REPO" --prerelease --latest=false \
    --title "EclipseOS package repository (x86_64)" \
    --notes "pacman repository. Point a [eclipseos] section at https://github.com/$GH_REPO/releases/download/$TAG. Signed by the EclipseOS packaging key (eclipseos-packaging.asc)."
fi

gh release upload "$TAG" -R "$GH_REPO" --clobber "$stage"/*

# Prune what the local repo no longer has (old package versions).
gh release view "$TAG" -R "$GH_REPO" --json assets -q '.assets[].name' |
  while read -r name; do
    [[ -e "$stage/$name" ]] || gh release delete-asset "$TAG" "$name" -R "$GH_REPO" -y
  done

echo "==> published $(ls "$stage" | wc -l) files to $GH_REPO release $TAG"
