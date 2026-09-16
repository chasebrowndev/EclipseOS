#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Install the session units into ~/.config/systemd/user, pointed at this
# checkout's debug binaries instead of /usr/bin. Development only — a real
# install drops the dist/ files in unmodified.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bin="$here/../target/debug"
# The userland is its own workspace with its own target dir (ADR 0042).
de_dist=$(CDPATH= cd -- "$here/../../EclipseDE/dist" && pwd)
de_bin="$de_dist/../target/debug"
dest="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$dest"

cp "$here/abyss-session.target" "$dest/"
for unit in eclipse-bar eclipse-toasts; do
    sed "s|/usr/bin/|$de_bin/|" "$de_dist/$unit.service" > "$dest/$unit.service"
done

# The apps a human launches. The bar, the toast stack and the launcher itself
# are session components, not applications, and deliberately have no entry.
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$apps"
cp "$de_dist"/applications/*.desktop "$apps/"

systemctl --user daemon-reload
systemctl --user enable eclipse-bar.service eclipse-toasts.service
echo "installed into $dest, ExecStart -> $de_bin"
