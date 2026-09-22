#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Install the session units into ~/.config/systemd/user, pointed at this
# checkout's debug binaries instead of /usr/bin. Development only — a real
# install drops the dist/ files in unmodified.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bin="$here/../target/debug"
dest="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$dest"

cp "$here/abyss-session.target" "$dest/"
for unit in hyperion eclipse-toasts eclipse-screensaver; do
    sed "s|/usr/bin/|$bin/|" "$here/$unit.service" > "$dest/$unit.service"
done

# The apps a human launches. The bar, the toast stack and the launcher itself
# are session components, not applications, and deliberately have no entry.
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$apps"
cp "$here"/applications/*.desktop "$apps/"

systemctl --user daemon-reload
systemctl --user enable hyperion.service eclipse-toasts.service eclipse-screensaver.service
echo "installed into $dest, ExecStart -> $bin"
