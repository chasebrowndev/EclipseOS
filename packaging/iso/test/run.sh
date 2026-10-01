#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Thin wrapper for the headless ISO test harness. Never builds an ISO, never signals any
# process except the QEMU whose pid it recorded itself.
#
#   run.sh selftest
#   run.sh unit
#   run.sh assert-no-keystroke /path/to/eclipseos.iso [--ref ref.png]
#   run.sh compare A.png B.png [--tolerance N]
#   run.sh [--work DIR] <any vm.py subcommand> ...      (see README.md)
#
# Work dir: --work, else $ECLIPSEOS_VM_WORK, else $TMPDIR/eclipseos-vm-UID (never in the repo).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
command -v python3 >/dev/null || { echo "run.sh: python3 not found" >&2; exit 2; }

case "${1:-}" in
  unit)    shift; exec python3 "$here/test_unit.py" "$@" ;;
  compare) shift; exec python3 "$here/compare.py" "$@" ;;
  ""|-h|--help) exec python3 "$here/vm.py" --help ;;
  *)       exec python3 "$here/vm.py" "$@" ;;
esac
