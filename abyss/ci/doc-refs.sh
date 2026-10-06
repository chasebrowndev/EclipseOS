#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Fail on a doc reference to a source path that no longer exists, or to a line
# past the end of the file. Only repo-rooted refs are checked (`abyss/...`,
# `shell/...`, ...); crate-relative shorthand like `state.rs:236` is ambiguous
# and left alone. Line numbers are checked for range, not content.
#
# The spec volumes (docs/ECLIPSEOS_SPECS_*) are skipped: their paths are
# templates and plans, not claims about the tree.
#
# Run from anywhere: abyss/ci/doc-refs.sh
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"

roots='abyss|shell|installer|fog|oracle-eyes|packaging|docs|decisions|assets|\.github'
bad=0
checked=0
while IFS=: read -r doc lineno ref; do
    ref=${ref//\`/}
    [[ $ref =~ ^([^:]+)(:([0-9]+))? ]] || continue
    path=${BASH_REMATCH[1]}
    line=${BASH_REMATCH[3]}
    # Brace and glob forms (`input/{mod,grabs}.rs`, `*.rs`) are not paths.
    [[ $path == *[{}*]* ]] && continue
    path=${path%/}
    # `shell/` is both a repo root and an ec-abyss module; docs use both.
    [[ ! -e $path && -e abyss/crates/ec-abyss/src/$path ]] && path=abyss/crates/ec-abyss/src/$path
    checked=$((checked + 1))
    if [[ ! -e $path ]]; then
        echo "$doc:$lineno: \`$ref\`: no such path"
        bad=$((bad + 1))
    elif [[ -n $line && -f $path ]]; then
        len=$(wc -l <"$path")
        if ((line > len)); then
            echo "$doc:$lineno: \`$ref\`: $path has $len lines"
            bad=$((bad + 1))
        fi
    fi
done < <(grep -noE "\`(${roots})/[^\` ]+\`" CLAUDE.md docs/[!E]*.md docs/internal/HANDOFF.md 2>/dev/null)

echo "doc-refs: $checked checked, $bad stale"
((bad == 0))
