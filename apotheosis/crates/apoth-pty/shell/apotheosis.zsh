# SPDX-License-Identifier: AGPL-3.0-only
# Apotheosis shell integration for zsh (T-14 §2). Takes effect inside Apotheosis panes
# only; `apoth shell-init zsh` prints it for manual sourcing.
# Prompt marks, title and cwd reports land with T-14 §2 delivery.
if [[ -n "${APOTHEOSIS_PANE-}" ]]; then
    # T-12 §5: `ap` is the short form of `apoth`.
    alias ap='apoth'
fi
