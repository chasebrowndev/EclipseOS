#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Build Abyss from a fresh clone and install it as a greeter-selectable session.
#
#     git clone https://github.com/chasebrowndev/EclipseOS.git
#     cd EclipseOS/AbyssCompositor
#     ./dist/install.sh
#
# Run it as your own user, NOT under sudo: the build has to use your cargo
# cache and your toolchain, and the script escalates with sudo only for the
# steps that write outside $HOME. It will tell you before each thing it
# installs from your distribution's package manager.
#
# This is the system install — real binaries in /usr/bin, units in
# /usr/lib/systemd/user, and nothing pointing back at the checkout, so the tree
# can be deleted afterwards. For a development install whose binaries symlink
# into target/release and go live on the next login, use install-session.sh.
#
# Flags:
#   --no-deps     skip the package-manager step (you have the libraries already)
#   --no-build    skip cargo, install what is already in target/release
#   --uninstall   remove everything this script installs, then exit
#   --yes         do not ask before installing packages
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)

BINS='abyss hyperion eclipse-toasts eclipse-center eclipse-launcher
      eclipse-settings eclipse-policy-viewer eclipse-ctl eclipse-screensaver eclipse-secret-prompt'
UNITS='hyperion.service eclipse-toasts.service eclipse-screensaver.service'

do_deps=1 do_build=1 assume_yes=0 uninstall=0
for arg in "$@"; do
    case "$arg" in
        --no-deps)   do_deps=0 ;;
        --no-build)  do_build=0 ;;
        --yes|-y)    assume_yes=1 ;;
        --uninstall) uninstall=1 ;;
        -h|--help)   sed -n '3,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)           echo "unknown flag: $arg (try --help)" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- uninstall --

if [ "$uninstall" -eq 1 ]; then
    say "removing Abyss"
    systemctl --user disable --now $UNITS 2>/dev/null || true
    # shellcheck disable=SC2086
    sudo rm -f $(for b in $BINS; do printf '/usr/bin/%s ' "$b"; done) \
               /usr/bin/abyss-session \
               /usr/share/wayland-sessions/abyss.desktop \
               $(for u in $UNITS; do printf '/usr/lib/systemd/user/%s ' "$u"; done) \
               /usr/lib/systemd/user/abyss-session.target \
               /usr/share/applications/eclipse-center.desktop \
               /usr/share/applications/eclipse-policy-viewer.desktop \
               /usr/share/applications/eclipse-settings.desktop
    systemctl --user daemon-reload 2>/dev/null || true
    echo "Removed. /etc/eclipse/ and your ~/.config/eclipse/ were left alone."
    exit 0
fi

# ------------------------------------------------------------- preflight --

[ "$(id -u)" -ne 0 ] || die "run me as your own user, not root — the build needs your cargo cache
             (the script calls sudo itself for the parts that need it)"
[ -f "$root/Cargo.toml" ] || die "run me from a checkout: $root/Cargo.toml is missing"
command -v sudo >/dev/null || die "sudo is required"

user=$(id -un)
home=$(getent passwd "$user" | cut -d: -f6)
[ -n "$home" ] || die "no home directory for $user"

# ---------------------------------------------------------------- packages --

# Runtime libraries and their headers. The build needs the headers; the session
# needs the libraries; on a rolling distro they are the same package.
arch_pkgs='rustup seatd libinput wayland libxkbcommon libdisplay-info mesa libdrm pkgconf'
deb_pkgs='rustup libseat-dev libinput-dev libwayland-dev libxkbcommon-dev
          libgbm-dev libegl-dev libudev-dev libdrm-dev libdisplay-info-dev pkg-config'
fed_pkgs='rustup libseat-devel libinput-devel wayland-devel libxkbcommon-devel
          libdisplay-info-devel mesa-libgbm-devel mesa-libEGL-devel systemd-devel
          libdrm-devel pkgconf-pkg-config'

if [ "$do_deps" -eq 1 ]; then
    if   command -v pacman  >/dev/null; then mgr=pacman; pkgs=$arch_pkgs
    elif command -v apt-get >/dev/null; then mgr=apt;    pkgs=$deb_pkgs
    elif command -v dnf     >/dev/null; then mgr=dnf;    pkgs=$fed_pkgs
    else
        mgr=none
        echo "Unknown distribution — install these yourself and re-run with --no-deps:" >&2
        echo "  libseat libinput wayland libxkbcommon libdisplay-info mesa libdrm libudev pkg-config" >&2
        die "no supported package manager found"
    fi

    say "packages to install with $mgr:"
    echo "  $(echo "$pkgs" | tr -s ' \n' ' ')"
    if [ "$assume_yes" -eq 0 ]; then
        printf 'Install them? [y/N] '
        read -r reply
        case "$reply" in [yY]*) ;; *) die "declined — re-run with --no-deps if you have them" ;; esac
    fi

    # shellcheck disable=SC2086
    case "$mgr" in
        pacman) sudo pacman -S --needed --noconfirm $pkgs ;;
        apt)    sudo apt-get update && sudo apt-get install -y $pkgs ;;
        dnf)    sudo dnf install -y $pkgs ;;
    esac
fi

# -------------------------------------------------------------------- build --

if [ "$do_build" -eq 1 ]; then
    command -v cargo >/dev/null || die "cargo is not on PATH.
             rustup was installed but its shims are not in this shell — open a
             new shell, or run: . \"\${CARGO_HOME:-\$HOME/.cargo}/env\""
    # rust-toolchain.toml pins the channel; rustup honours it without a flag.
    say "building (this takes a few minutes on a cold cache)"
    ( cd "$root" && cargo build --release --workspace --bins )
fi

bin="$root/target/release"
for b in $BINS; do
    [ -x "$bin/$b" ] || die "missing $bin/$b — drop --no-build, or run
             cargo build --release --workspace --bins"
done

# ------------------------------------------------------------------ install --

say "installing to /usr"

# 1. Binaries. Real files: the checkout is not needed after this point.
# shellcheck disable=SC2086
sudo install -Dm 0755 -t /usr/bin $(for b in $BINS; do printf '%s ' "$bin/$b"; done)

# 2. The login wrapper. It sets the environment that is knowable before the
#    compositor runs; the systemd/D-Bus handoff happens inside abyss --session,
#    because WAYLAND_DISPLAY does not exist until the socket does (ADR 0032).
sudo install -Dm 0755 "$here/abyss-session" /usr/bin/abyss-session

# 3. The session entry the greeter lists. greetd/regreet, GDM, SDDM and LightDM
#    all build their session menu by scanning this directory — no greeter
#    configuration is needed, and the entry already points at /usr/bin.
sudo install -Dm 0644 "$here/abyss.desktop" /usr/share/wayland-sessions/abyss.desktop

# 4. User units, system-wide so every account on the box gets them. These are
#    shipped pointing at /usr/bin, which is where step 1 put the binaries.
sudo install -Dm 0644 -t /usr/lib/systemd/user \
    "$here/abyss-session.target" "$here/hyperion.service" "$here/eclipse-toasts.service" \
    "$here/eclipse-screensaver.service"

# 5. The apps a human launches. The bar, toasts and launcher are session
#    components, not applications, and deliberately have no entry.
sudo install -Dm 0644 -t /usr/share/applications "$here"/applications/*.desktop

# ---------------------------------------------------------------- seat/perms --

# libseat prefers logind and falls back to seatd. On a systemd box logind is
# already there and nothing is needed; elsewhere seatd has to be running and
# the user has to be in its group, or the compositor cannot open the GPU.
if [ -d /run/systemd/system ] && command -v loginctl >/dev/null; then
    say "seat: logind — nothing to do"
else
    say "seat: no logind, enabling seatd"
    sudo systemctl enable --now seatd.service 2>/dev/null \
        || echo "  could not enable seatd.service — start it yourself before logging in" >&2
    if getent group seat >/dev/null && ! id -nG "$user" | grep -qw seat; then
        sudo usermod -aG seat "$user"
        echo "  added $user to the 'seat' group — log out fully for it to take effect"
    fi
fi

# The DRM and libinput device nodes. logind hands these over per-session, so
# this only matters on the seatd path, but joining is harmless either way.
for grp in video input; do
    if getent group "$grp" >/dev/null && ! id -nG "$user" | grep -qw "$grp"; then
        sudo usermod -aG "$grp" "$user"
        echo "  added $user to the '$grp' group"
    fi
done

# ------------------------------------------------------------------ enable --

say "enabling the session's own services for $user"
systemctl --user daemon-reload
systemctl --user enable $UNITS

cat <<EOF

Abyss is installed.

  Log out, then pick "Abyss" from the session menu at your greeter.

The compositor runs with built-in defaults; it needs no configuration file.
To change one, write ~/.config/eclipse/abyss.kdl — Super+Escape is the trusted-UI
override chord and is reserved, everything else is rebindable.

If the session fails to start, log in to a text console (Ctrl+Alt+F2) and read:

  journalctl --user -t abyss -b

This checkout is no longer referenced by anything and can be deleted.
EOF
