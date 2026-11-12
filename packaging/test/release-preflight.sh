#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# What the release-repo build needs, checked in seconds instead of 25 minutes:
#   1. every workspace's Cargo.lock is current (the PKGBUILD builds --locked;
#      `cargo update -w` misses path dependencies from other workspaces);
#   2. every PKGBUILD makedepend is in release-repo.yml's own package list
#      (that build runs makepkg with dependency checks off).
#
#   packaging/test/release-preflight.sh
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
pkgbuild="$root/packaging/pkg/eclipseos/PKGBUILD"
workflow="$root/.github/workflows/release-repo.yml"
fail=0

for ws in abyss shell installer oracle-eyes fog portal; do
  if ! (cd "$root/$ws" && cargo metadata --format-version 1 --locked >/dev/null 2>&1); then
    echo "FAIL $ws/Cargo.lock is stale: run 'cargo metadata --format-version 1 >/dev/null' in $ws/ and commit it"
    fail=1
  fi
done

# The workflow installs rust through rustup and checks out with git.
provided="rust cargo git"
makedepends=$(sed -n 's/^makedepends=(\(.*\))/\1/p' "$pkgbuild")
installed=$(sed -n '/name: Install build dependencies/,/useradd/p' "$workflow")
for dep in $makedepends; do
  case " $provided " in *" $dep "*) continue ;; esac
  if ! grep -qw -- "$dep" <<<"$installed"; then
    echo "FAIL makedepend '$dep' is not installed by release-repo.yml (Install build dependencies)"
    fail=1
  fi
done

[ "$fail" = 0 ] && echo "release preflight ok"
exit "$fail"
