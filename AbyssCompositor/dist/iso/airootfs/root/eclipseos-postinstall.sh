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

# --- wifi ---------------------------------------------------------------------
# archinstall can leave NetworkManager configured with the iwd backend while
# iwd.service itself is disabled, and NM starting before iwd owns the device
# means the machine boots with no wifi until someone connects by hand. If the
# iwd backend is configured, make sure iwd is enabled and NM is ordered behind
# it.
if grep -rqs 'wifi.backend *= *iwd' /mnt/etc/NetworkManager/conf.d/; then
  say "ordering NetworkManager behind iwd"
  install -Dm0644 /dev/stdin /mnt/etc/systemd/system/NetworkManager.service.d/10-iwd-first.conf <<'NMORDER'
[Unit]
Wants=iwd.service
After=iwd.service
NMORDER
  arch-chroot /mnt systemctl enable iwd
fi

# --- boot menu branding -------------------------------------------------------
# archinstall writes its own limine.conf and names every entry "Arch Linux".
# The machine is EclipseOS by the time this script is done, so the boot menu
# should say so. Only the entry titles are touched -- paths, cmdline and the
# rest are archinstall's and are none of our business.
for conf in /mnt/boot/limine.conf /mnt/boot/limine/limine.conf /mnt/boot/EFI/limine/limine.conf \
            /mnt/boot/EFI/BOOT/limine.conf; do
  [[ -f $conf ]] || continue
  say "branding the boot menu in ${conf#/mnt}"
  sed -i -E 's,^(/+)Arch Linux,\1EclipseOS,' "$conf"
  # Seamless boot: no menu, no kernel or systemd text. With nothing written to
  # the console, fbcon's deferred takeover leaves the UKI splash up until the
  # greeter takes the display.
  sed -i -E 's/^timeout:.*/timeout: 0/' "$conf"
  grep -q '^timeout:' "$conf" || sed -i '1i timeout: 0' "$conf"
  sed -i -E '/^\s*cmdline:/{/ quiet( |$)/!s/$/ quiet loglevel=3 systemd.show_status=auto rd.udev.log_level=3 vt.global_cursor_default=0/}' "$conf"
done

# A UKI carries its splash; the preset archinstall generated embeds Arch's.
rebuild=0
presets=(/mnt/etc/mkinitcpio.d/*.preset)
if grep -qs 'splash-arch.bmp' "${presets[@]}"; then
  say "branding the boot splash"
  sed -i 's,/usr/share/systemd/bootctl/splash-arch.bmp,/usr/share/eclipseos/splash.bmp,' "${presets[@]}"
  rebuild=1
fi
# The busybox initramfs prints its own lines (fsck's "clean") whatever the
# cmdline says; the systemd one honours `quiet`.
if grep -Eqs '^HOOKS=\(base udev' /mnt/etc/mkinitcpio.conf; then
  say "switching the initramfs to systemd"
  sed -i -E '/^HOOKS=/{s/\bbase udev\b/systemd/;s/\bkeymap consolefont\b/sd-vconsole/}' /mnt/etc/mkinitcpio.conf
  rebuild=1
fi
if ((rebuild)); then
  arch-chroot /mnt mkinitcpio -P
fi

say "done. reboot and pick Abyss at the greeter."
say "if the greeter does not come up: Ctrl+Alt+F2, then"
say "  journalctl -u greetd -b"
