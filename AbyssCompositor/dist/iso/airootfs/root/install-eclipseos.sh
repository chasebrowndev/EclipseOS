#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# EclipseOS installer (D-03 §4). Deliberately small and readable: it partitions
# one disk, pacstraps the same package set the live medium booted with, and
# installs a bootloader. There is no setup system and no first-boot wizard --
# the shipped defaults in /etc/eclipse are the configuration (D-01 §6).
#
# `archinstall` is also on the medium for anyone who wants the guided route.
# This script exists because its JSON schema moves between releases and a
# forty-line pacstrap does not.
#
# DESTRUCTIVE. It wipes the disk you name. It asks first, twice.
set -euo pipefail

die() { printf '\n!! %s\n' "$*" >&2; exit 1; }
say() { printf '\n==> %s\n' "$*"; }

[[ $EUID -eq 0 ]] || die "run as root"
[[ -d /sys/firmware/efi ]] || die "UEFI boot required; this medium was booted in BIOS mode"

# The EclipseOS packages ride on this medium, but Arch's own do not. Check
# before touching the disk rather than half an hour into a partitioned one.
if ! curl -fsS --max-time 10 -o /dev/null https://geo.mirror.pkgbuild.com/lastupdate; then
  cat >&2 <<'NET'

!! no internet. The EclipseOS packages are on this medium, but the Arch base
   system is not, so the install needs a connection.

   wifi:      iwctl
                station wlan0 scan
                station wlan0 get-networks
                station wlan0 connect <SSID>
                exit
   ethernet:  plug it in; systemd-networkd takes DHCP on its own

   then run this script again.
NET
  exit 1
fi

# --- target disk -------------------------------------------------------------
say "disks on this machine"
lsblk -dpno NAME,SIZE,MODEL | grep -v loop

# No disks at all is a firmware problem, not a typo. Laptops that ship with
# the SATA/NVMe controller in RAID or Intel VMD mode hide the drive from Linux
# entirely, and the error people hit downstream is a confusing "not a block
# device" on a path they read off the machine's own sticker.
if [[ -z "$(lsblk -dpno NAME | grep -v loop)" ]]; then
  die "no disks visible. If this machine has an NVMe drive, its firmware is \
probably set to RAID/Intel VMD -- switch the SATA/NVMe mode to AHCI/NVMe in \
the BIOS and boot this medium again."
fi

read -rp $'\nfull path of the disk to install to (e.g. /dev/nvme0n1): ' DISK
DISK="${DISK//[[:space:]]/}"
# "nvme0n1" is what the list reads like at a glance, and it is what people
# type. Accept it rather than failing on a missing /dev/.
[[ $DISK == /* ]] || DISK="/dev/$DISK"
[[ -b $DISK ]] || die "$DISK is not a block device. Pick one of the paths listed above."

read -rp "this ERASES everything on $DISK. type the disk path again to confirm: " CONFIRM
[[ $CONFIRM == "$DISK" ]] || die "confirmation did not match; nothing was changed"

read -rp $'\nhostname: ' HOSTNAME
read -rp 'username (added to wheel): ' USERNAME
[[ -n $HOSTNAME && -n $USERNAME ]] || die "hostname and username are both required"

# nvme0n1 -> nvme0n1p1; sda -> sda1
part() { [[ $DISK == *nvme* || $DISK == *mmcblk* ]] && echo "${DISK}p$1" || echo "${DISK}$1"; }
ESP="$(part 1)"
ROOT="$(part 2)"

# --- partition ---------------------------------------------------------------
say "partitioning $DISK"
sgdisk --zap-all "$DISK"
sgdisk -n1:0:+1G  -t1:ef00 -c1:EFI \
       -n2:0:0    -t2:8304 -c2:eclipseos "$DISK"
partprobe "$DISK"
udevadm settle

mkfs.fat -F32 -n ECLIPSE_ESP "$ESP"
mkfs.ext4 -F -L eclipseos "$ROOT"

mount "$ROOT" /mnt
mount --mkdir "$ESP" /mnt/boot

# --- base system -------------------------------------------------------------
# The live medium's own package list, minus the archiso-only pieces. Installing
# exactly what booted is the point: what you just tried is what you get.
say "installing packages (this is the long part)"
mapfile -t PKGS < <(grep -vE '^\s*(#|$)' /root/eclipseos-packages.txt)

# The EclipseOS packages come off the medium itself, not the tailnet: this
# laptop is not on the tailnet until NetworkManager exists, which is one of
# the packages. Arch's own packages still come from the mirrors, so the
# install needs the network -- it just does not need chase-pc.
# The installed system gets the tailnet drop-in instead, further down.
KEYID="$(gpg --show-keys --with-colons /root/eclipseos-packaging.asc 2>/dev/null |
         awk -F: '/^fpr:/{print $10; exit}')"
PACCONF=/root/eclipseos-install-pacman.conf
cp /etc/pacman.conf "$PACCONF"
if [[ -d /root/eclipseos-repo ]]; then
  # The packages are signed by the EclipseOS packaging key. TrustAll is not
  # enough on its own -- pacman cannot verify against a key it does not hold,
  # so import it here and again into the installed system further down.
  pacman-key --add /root/eclipseos-packaging.asc
  pacman-key --lsign-key "$KEYID"
  cat >>"$PACCONF" <<'REPO'

# Baked into the install medium by dist/iso/build-iso.sh (D-03). The database
# here is unsigned (rebuilt at bake time); the packages in it are signed.
[eclipseos]
SigLevel = PackageRequired DatabaseOptional
Server = file:///root/eclipseos-repo
REPO
else
  say "WARNING: /root/eclipseos-repo is missing -- this medium was built"
  say "         without the EclipseOS packages. Falling back to the tailnet."
fi

pacstrap -K -C "$PACCONF" /mnt "${PKGS[@]}"
genfstab -U /mnt >>/mnt/etc/fstab

# --- configure ---------------------------------------------------------------
say "configuring the installed system"
echo "$HOSTNAME" >/mnt/etc/hostname
# Asked, not assumed. A wrong timezone is silent -- it looks like the clock is
# just wrong -- and it is the sort of thing nobody goes back to fix.
read -rp $'timezone [America/New_York]: ' TZ
TZ="${TZ//[[:space:]]/}"
TZ="${TZ:-America/New_York}"
[[ -f /usr/share/zoneinfo/$TZ ]] || die "$TZ is not a timezone. See: timedatectl list-timezones"
ln -sf "/usr/share/zoneinfo/$TZ" /mnt/etc/localtime

# The live medium's wifi lives in iwd; the installed system runs NetworkManager
# and would boot with no saved networks at all. Carry them over.
if [[ -d /var/lib/iwd ]] && compgen -G '/var/lib/iwd/*.psk' >/dev/null; then
  install -dm0700 /mnt/var/lib/iwd
  cp -a /var/lib/iwd/*.psk /mnt/var/lib/iwd/
  # Those files are iwd's, and NetworkManager only reads them when iwd is its
  # backend -- with the default wpa_supplicant backend they are dead weight and
  # the laptop boots with no known networks.
  install -Dm0644 /dev/stdin /mnt/etc/NetworkManager/conf.d/wifi-backend.conf <<'NMCONF'
[device]
wifi.backend=iwd
NMCONF
  say "carried $(compgen -G '/var/lib/iwd/*.psk' | wc -l) saved wifi network(s) over"
fi
sed -i 's/^#en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/' /mnt/etc/locale.gen
echo 'LANG=en_US.UTF-8' >/mnt/etc/locale.conf

# The EclipseOS repo drop-in ships in eclipseos-meta; pacman.conf has to include
# it (D-02). Adding the line here rather than in the package keeps the package
# from editing a file it does not own.
# The installed system's drop-in is `Required DatabaseRequired` over the
# tailnet, so its own keyring needs the packaging key too or the first
# `pacman -Syu` fails the same way the ISO build did.
if [[ -f /root/eclipseos-packaging.asc ]]; then
  install -Dm0644 /root/eclipseos-packaging.asc /mnt/root/eclipseos-packaging.asc
fi

grep -q 'eclipseos.conf' /mnt/etc/pacman.conf ||
  printf '\n# EclipseOS packages (D-02)\nInclude = /etc/pacman.d/eclipseos.conf\n' >>/mnt/etc/pacman.conf

arch-chroot /mnt /bin/bash -euo pipefail <<CHROOT
if [[ -f /root/eclipseos-packaging.asc ]]; then
  pacman-key --add /root/eclipseos-packaging.asc
  pacman-key --lsign-key "$KEYID"
  rm -f /root/eclipseos-packaging.asc
fi
locale-gen
hwclock --systohc
mkinitcpio -P

useradd -m -G wheel -s /bin/bash "$USERNAME"
echo '%wheel ALL=(ALL:ALL) ALL' >/etc/sudoers.d/10-wheel
chmod 0440 /etc/sudoers.d/10-wheel

# D-01 §5: no group management. A logind session on a seat is the whole
# requirement; logind's ACL on the DRM node does the rest.

systemctl enable greetd NetworkManager iwd bluetooth systemd-timesyncd
CHROOT

# --- bootloader ---------------------------------------------------------------
# Limine. Not systemd-boot -- this firmware refused its stub on the install
# medium and there is no reason to expect better from the internal disk -- and
# not GRUB, whose generated config is a script nobody reads and whose failures
# are correspondingly hard to read. Limine's UEFI install is two files: the
# stub at the removable fallback path, which needs no NVRAM entry to be found,
# and a config that says exactly what it does.
say "installing Limine"
install -Dm0644 /mnt/usr/share/limine/BOOTX64.EFI /mnt/boot/EFI/BOOT/BOOTX64.EFI

# The ESP is mounted at /boot, so the kernel and initramfs sit at the root of
# the volume Limine boots from -- that is what `boot():/` resolves to.
# amd_pstate=active is the Framework 13 AMD default worth having from the first
# boot rather than discovering later (D-01 §2).
ROOT_UUID="$(blkid -s UUID -o value "$ROOT")"
UCODE=""
[[ -f /mnt/boot/amd-ucode.img ]] && UCODE="module_path: boot():/amd-ucode.img"
cat >/mnt/boot/limine.conf <<LIMINE
timeout: 2

/EclipseOS
    protocol: linux
    path: boot():/vmlinuz-linux
    cmdline: root=UUID=$ROOT_UUID rw amd_pstate=active
    $UCODE
    module_path: boot():/initramfs-linux.img

/EclipseOS (fallback initramfs)
    protocol: linux
    path: boot():/vmlinuz-linux
    cmdline: root=UUID=$ROOT_UUID rw
    module_path: boot():/initramfs-linux-fallback.img
LIMINE

# Nothing else refreshes the copy on the ESP, so a `limine` upgrade would leave
# the machine booting last release's stub indefinitely.
install -Dm0644 /dev/stdin /mnt/etc/pacman.d/hooks/95-limine-esp.hook <<'HOOK'
[Trigger]
Type = Path
Operation = Install
Operation = Upgrade
Target = usr/share/limine/BOOTX64.EFI

[Action]
Description = Copying the Limine stub to the ESP...
When = PostTransaction
Exec = /usr/bin/install -Dm0644 /usr/share/limine/BOOTX64.EFI /boot/EFI/BOOT/BOOTX64.EFI
HOOK

# The medium's database is unsigned -- it is rebuilt at bake time by a root
# build that does not hold the packaging secret key -- but the drop-in the
# installed system uses is `DatabaseRequired` against the signed tailnet copy.
# Leaving the medium's db in the sync cache means every later pacman run
# validates that stale unsigned file and fails with "missing required
# signature", including the first `pacman -S` the owner ever types. Drop it so
# the first `-Sy` fetches the signed one.
rm -f /mnt/var/lib/pacman/sync/eclipseos.db*

say "set a password for root"
arch-chroot /mnt passwd
say "set a password for $USERNAME"
arch-chroot /mnt passwd "$USERNAME"

umount -R /mnt
say "done. reboot, pick Abyss at the greeter."
