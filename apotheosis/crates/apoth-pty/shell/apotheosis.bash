# SPDX-License-Identifier: AGPL-3.0-only
# Apotheosis shell integration for bash (T-14 §2). Takes effect inside Apotheosis panes
# only; `apoth shell-init bash` prints it for manual sourcing.
# Prompt marks, title and cwd reports land with T-14 §2 delivery.
if [ -n "${APOTHEOSIS_PANE-}" ]; then
    # T-12 §5: `ap` is the short form of `apoth`.
    alias ap='apoth'
    # T-22 §3: `security.sudo-askpass` armed this pane; interactive shells only.
    case $- in
        *i*) if [ -n "${SUDO_ASKPASS-}" ] && [ -n "${APOTHEOSIS_ASKPASS_SOCK-}" ]; then
                alias sudo='sudo -A'
            fi ;;
    esac
fi
