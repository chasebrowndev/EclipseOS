#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Install the session units into ~/.config/systemd/user, pointed at this
# checkout's debug binaries instead of /usr/bin. Development only — a real
# install drops the packaging/ files in unmodified.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bin="$here/../target/debug"
dest="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$dest"

cp "$here/abyss-session.target" "$dest/"
for unit in ec-hyperion-bar ec-toasts ec-wallpaper ec-screensaver ec-pairing ec-portal; do
    sed "s|/usr/bin/|$bin/|" "$here/$unit.service" > "$dest/$unit.service"
done

# The colour-scheme portal backend (ADR 0084), bus-activated. xdg-desktop-portal
# reads backend .portal files only from its system directory, so that one file
# stays with packaging/install-session.sh; the service itself is callable here.
services="${XDG_DATA_HOME:-$HOME/.local/share}/dbus-1/services"
mkdir -p "$services"
sed "s|/usr/bin/|$bin/|" "$here/dbus/org.freedesktop.impl.portal.desktop.abyss.service" \
    > "$services/org.freedesktop.impl.portal.desktop.abyss.service"

# The apps a human launches. The bar, the toast stack and the launcher itself
# are session components, not applications, and deliberately have no entry.
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$apps"
cp "$here"/applications/*.desktop "$apps/"

systemctl --user daemon-reload
systemctl --user enable ec-hyperion-bar.service ec-toasts.service ec-wallpaper.service ec-screensaver.service \
    ec-pairing.service
echo "installed into $dest, ExecStart -> $bin"
