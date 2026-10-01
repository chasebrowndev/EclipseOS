#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Install Abyss as a greeter-selectable session, resolved through symlinks into
# this checkout's release build. Run with sudo:
#
#     sudo ./packaging/install-session.sh
#
# Development install by design: /usr/local/bin/* are symlinks, so a rebuild is
# live at the next login and nothing has to be reinstalled. A distribution
# install copies packaging/abyss.desktop and packaging/abyss-session in unmodified and
# ships real binaries in /usr/bin instead.
set -eu

[ "$(id -u)" -eq 0 ] || { echo "run me with sudo" >&2; exit 1; }
user="${SUDO_USER:-}"
[ -n "$user" ] || { echo "SUDO_USER unset — run via sudo, not as a root login" >&2; exit 1; }

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bin="$here/../target/release"
home=$(getent passwd "$user" | cut -d: -f6)

for b in ec-abyss ec-hyperion-bar ec-toasts ec-wallpaper ec-center ec-launcher \
         ec-settings ec-policy-viewer ec-ctl ec-screensaver ec-secret-prompt \
         ec-pairing; do
    [ -x "$bin/$b" ] || { echo "missing $bin/$b — cargo build --release --workspace --bins" >&2; exit 1; }
done

# 1. Binaries. Symlinks, so the session always runs what was last built.
install -d /usr/local/bin
for b in ec-abyss ec-hyperion-bar ec-toasts ec-wallpaper ec-center ec-launcher \
         ec-settings ec-policy-viewer ec-ctl ec-screensaver ec-secret-prompt \
         ec-pairing; do
    ln -sfn "$bin/$b" "/usr/local/bin/$b"
done
install -m 0755 "$here/abyss-session" /usr/local/bin/abyss-session

# 2. The session entry the greeter lists. Exec is the wrapper, not the binary:
#    XDG_SESSION_TYPE and friends have to be set before the compositor starts.
install -d /usr/share/wayland-sessions
install -m 0644 "$here/abyss.desktop" /usr/share/wayland-sessions/abyss.desktop
sed -i 's|^Exec=.*|Exec=/usr/local/bin/abyss-session|' /usr/share/wayland-sessions/abyss.desktop

# 3. The user units ec-abyss --session pulls in once its socket exists. These are
#    the shipped files with /usr/bin rewritten to /usr/local/bin to match (1).
dest="$home/.config/systemd/user"
install -d -o "$user" -g "$user" "$dest"
install -m 0644 -o "$user" -g "$user" "$here/abyss-session.target" "$dest/abyss-session.target"
for unit in ec-hyperion-bar ec-toasts ec-wallpaper ec-screensaver ec-pairing; do
    sed 's|/usr/bin/|/usr/local/bin/|' "$here/$unit.service" > "$dest/$unit.service"
    chown "$user:$user" "$dest/$unit.service"
done

# 3b. The taskbar's add-on manifest (ADR 0066). Hosts read manifests only from
#     /usr/share/eclipse/addons, never a user directory, so it is copied there.
install -Dm 0644 "$here/addons/hyperion.kdl" /usr/share/eclipse/addons/hyperion.kdl
#     Its premade command widgets, approved by shipping (ADR 0067).
install -Dm 0644 -t /usr/share/eclipse/widgets "$here"/widgets/*.kdl

# 4. The apps a human launches. The bar, toasts and launcher are session
#    components, not applications, and deliberately have no entry.
apps="$home/.local/share/applications"
install -d -o "$user" -g "$user" "$apps"
for f in "$here"/applications/*.desktop; do
    install -m 0644 -o "$user" -g "$user" "$f" "$apps/"
done

# 5. Reload and enable the user units. sudo drops XDG_RUNTIME_DIR and the bus
#    address, so a bare `runuser -u ... systemctl --user` has no bus to reach
#    and, under `set -e`, ended the installer after every file was in place. Hand
#    it the user's own runtime directory. A user with no running systemd
#    instance (installing from a bare TTY) is not a failure: the units are
#    already written and are read when their manager next starts.
uid=$(id -u "$user")
rt="/run/user/$uid"
units="ec-hyperion-bar.service ec-toasts.service ec-wallpaper.service ec-screensaver.service ec-pairing.service"
if [ -S "$rt/bus" ]; then
    as_user() {
        runuser -u "$user" -- env XDG_RUNTIME_DIR="$rt" DBUS_SESSION_BUS_ADDRESS="unix:path=$rt/bus" "$@"
    }
    as_user systemctl --user daemon-reload
    # shellcheck disable=SC2086 # $units is a list of unit names on purpose
    as_user systemctl --user enable $units
else
    echo "No systemd user session for $user right now: the units are installed but not enabled." >&2
    echo "Once logged in, as $user: systemctl --user daemon-reload && systemctl --user enable $units" >&2
fi

echo "Abyss installed. Log out and pick 'Abyss' in the greeter session menu."
echo "Binaries symlink to $bin — rebuild there and the next login picks it up."
