# SPDX-License-Identifier: AGPL-3.0-only
# Apotheosis shell integration for fish (T-14 §2). Takes effect inside Apotheosis panes
# only; `apoth shell-init fish` prints it for manual sourcing.
# Prompt marks, title and cwd reports land with T-14 §2 delivery.
if set -q APOTHEOSIS_PANE; and test -n "$APOTHEOSIS_PANE"
    # T-12 §5: `ap` is the short form of `apoth`.
    function ap --wraps apoth --description 'apoth (Apotheosis)'
        apoth $argv
    end
end
