#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# Add EclipseOS to a system `archinstall` just installed (D-03 §4).
#
# The fast path is install-eclipseos.sh. This is the other one: let archinstall
# do the disk -- encryption, btrfs, dual boot, whatever it offers -- and bolt
# EclipseOS onto the result before the first reboot. It does exactly the parts
# archinstall cannot know about: the signed repo on this medium, its key, and
# the greeter.
#
# Run it from the live medium with the new system still mounted, or point it at
# the root partition and it will mount it for you.
set -euo pipefail

die() { printf '\n!! %s\n' "$*" >&2; exit 1; }
say() { printf '\n==> %s\n' "$*"; }

[[ $EUID -eq 0 ]] || die "run as root"

# --- find the target ---------------------------------------------------------
if ! mountpoint -q /mnt; then
  say "nothing mounted at /mnt"
  lsblk -dpno NAME,SIZE,MODEL | grep -v loop
  lsblk -pno NAME,SIZE,FSTYPE,LABEL | grep -v loop
  read -rp $'\nroot partition of the system you just installed (e.g. /dev/nvme0n1p2): ' ROOT
  ROOT="${ROOT//[[:space:]]/}"
  [[ $ROOT == /* ]] || ROOT="/dev/$ROOT"
  [[ -b $ROOT ]] || die "$ROOT is not a block device"
  mount "$ROOT" /mnt
  # archinstall's ESP. Without it a kernel or bootloader update has nowhere to
  # write, and pacman hooks fail in ways that are hard to read.
  if ! mountpoint -q /mnt/boot; then
    read -rp 'ESP (e.g. /dev/nvme0n1p1), or blank if /boot is on root: ' ESP
    ESP="${ESP//[[:space:]]/}"
    if [[ -n $ESP ]]; then
      [[ $ESP == /* ]] || ESP="/dev/$ESP"
      mount --mkdir "$ESP" /mnt/boot
    fi
  fi
fi

[[ -d /mnt/etc ]] || die "/mnt does not look like an installed system (no /etc)"
[[ -d /root/eclipseos-repo ]] || die "this medium has no /root/eclipseos-repo"

# --- trust the packaging key on the live medium ------------------------------
KEYID="$(gpg --show-keys --with-colons /root/eclipseos-packaging.asc |
         awk -F: '/^fpr:/{print $10; exit}')"
say "importing the EclipseOS packaging key ($KEYID)"
pacman-key --add /root/eclipseos-packaging.asc
pacman-key --lsign-key "$KEYID"

PACCONF=/root/eclipseos-post-pacman.conf
cp /etc/pacman.conf "$PACCONF"
cat >>"$PACCONF" <<'REPO'

[eclipseos]
SigLevel = PackageRequired DatabaseOptional
Server = file:///root/eclipseos-repo
REPO

# --- install ------------------------------------------------------------------
say "installing EclipseOS and the greeter"
pacstrap -K -C "$PACCONF" /mnt eclipseos-meta greetd greetd-regreet cage

# The installed system talks to the repo over the tailnet, so it needs the key
# in its own keyring too -- the same trap the ISO build hit.
install -Dm0644 /root/eclipseos-packaging.asc /mnt/root/eclipseos-packaging.asc
grep -q 'eclipseos.conf' /mnt/etc/pacman.conf ||
  printf '\n# EclipseOS packages (D-02)\nInclude = /etc/pacman.d/eclipseos.conf\n' \
    >>/mnt/etc/pacman.conf

arch-chroot /mnt /bin/bash -euo pipefail <<CHROOT
pacman-key --add /root/eclipseos-packaging.asc
pacman-key --lsign-key "$KEYID"
rm -f /root/eclipseos-packaging.asc
systemctl enable greetd
CHROOT

say "done. reboot and pick Abyss at the greeter."
say "if the greeter does not come up: Ctrl+Alt+F2, then"
say "  journalctl -u greetd -b"
