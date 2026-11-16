# SPDX-License-Identifier: AGPL-3.0-only
# Apotheosis shell integration for fish (T-14 §2). Takes effect inside Apotheosis panes
# only; `apoth shell-init fish` prints it for manual sourcing.
# Prompt marks, title and cwd reports land with T-14 §2 delivery.
if set -q APOTHEOSIS_PANE; and test -n "$APOTHEOSIS_PANE"
    # T-12 §5: `ap` is the short form of `apoth`.
    function ap --wraps apoth --description 'apoth (Apotheosis)'
        apoth $argv
    end
    # T-22 §3: `security.sudo-askpass` armed this pane; interactive shells only.
    if status is-interactive; and set -q SUDO_ASKPASS; and set -q APOTHEOSIS_ASKPASS_SOCK
        function sudo --wraps sudo --description 'sudo -A (Apotheosis password prompt)'
            command sudo -A $argv
        end
    end
end
