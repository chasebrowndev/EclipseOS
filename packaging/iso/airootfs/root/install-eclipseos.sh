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

# --- leftovers from an earlier attempt --------------------------------------
# A failed run leaves the target mounted, and gpg daemons started by pacstrap -K
# keep it busy. Release both, so running this script again just works instead of
# failing on "target is busy" until someone unmounts by hand. No lazy unmount.
stop_target_daemons() {
  local sig p comm
  for sig in TERM KILL; do
    local found=0
    for p in /proc/[0-9]*; do
      comm="$(cat "$p/comm" 2>/dev/null)" || continue
      case $comm in gpg-agent|dirmngr|keyboxd|scdaemon) ;; *) continue ;; esac
      if [[ "$(readlink "$p/root" 2>/dev/null)" == /mnt* ]] ||
         tr '\0' ' ' <"$p/cmdline" 2>/dev/null | grep -q -- '--homedir /mnt'; then
        found=1
        kill "-$sig" "${p#/proc/}" 2>/dev/null || true
      fi
    done
    (( found )) || return 0
    sleep 1
  done
}

unmount_target() {
  local i
  stop_target_daemons
  sync
  for i in 1 2 3 4 5; do
    mountpoint -q /mnt || return 0
    umount -R /mnt 2>/dev/null && return 0
    sleep 1
  done
  mountpoint -q /mnt && return 1
  return 0
}

reclaim_leftovers() {
  local mp
  if mountpoint -q /mnt; then
    say "/mnt is still mounted from an earlier attempt; unmounting it"
    unmount_target || die "could not unmount /mnt (something is using it). See: findmnt -R /mnt ; fuser -vm /mnt"
  fi
  # Any other mount from the chosen disk would make the wipe below fail.
  while read -r mp; do
    [[ $mp == /* ]] || continue
    say "unmounting $mp (on $DISK)"
    umount -R "$mp" || die "could not unmount $mp; unmount it and run this script again"
  done < <(lsblk -nrpo MOUNTPOINTS "$DISK" | tr ' ' '\n' | sort -r)
}
reclaim_leftovers

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
# A slow or filtered link must not abort a transaction ("Operation too slow",
# pacman's low-speed timeout), and many parallel streams over a bad link starve
# each other. Applies to pacstrap and to the pacman calls below via this config.
sed -i -E '/^[[:space:]]*#?[[:space:]]*(DisableDownloadTimeout|ParallelDownloads)([[:space:]=]|$)/d' "$PACCONF"
sed -i '/^\[options\]/a DisableDownloadTimeout\nParallelDownloads = 3' "$PACCONF"
if [[ -d /opt/eclipseos-repo ]]; then
  # The packages are signed by the EclipseOS packaging key. TrustAll is not
  # enough on its own -- pacman cannot verify against a key it does not hold,
  # so import it here and again into the installed system further down.
  pacman-key --add /root/eclipseos-packaging.asc
  pacman-key --lsign-key "$KEYID"
  cat >>"$PACCONF" <<'REPO'

# Baked into the install medium by packaging/iso/build-iso.sh (D-03). The database
# here is unsigned (rebuilt at bake time); the packages in it are signed.
[eclipseos]
SigLevel = PackageRequired DatabaseOptional
Server = file:///opt/eclipseos-repo
REPO
else
  say "WARNING: /opt/eclipseos-repo is missing -- this medium was built"
  say "         without the EclipseOS packages. Falling back to the tailnet."
fi

# D-01 §2.3: NVIDIA Turing or newer (PCI device id >= 0x1e00) gets the open
# kernel module via DKMS; older NVIDIA stays on nouveau. Sysfs only.
NVIDIA_OPEN=0
for d in /sys/bus/pci/devices/*; do
  [[ -r $d/class && -r $d/vendor && -r $d/device ]] || continue
  v="$(<"$d/vendor")"; dev="$(<"$d/device")"
  [[ $v =~ ^0x[0-9a-fA-F]{4}$ && $dev =~ ^0x[0-9a-fA-F]{4}$ ]] || continue
  [[ $(<"$d/class") == 0x03* && ${v,,} == 0x10de ]] || continue
  if (( dev >= 0x1e00 )); then NVIDIA_OPEN=1; fi
done
if (( NVIDIA_OPEN )); then
  say "NVIDIA Turing or newer detected: installing nvidia-open-dkms"
  PKGS+=(nvidia-open-dkms nvidia-utils linux-headers)
  PKGS+=(linux-lts-headers)
fi

# D-01 §2: a second kernel to fall back to (not on the live medium itself).
PKGS+=(linux-lts)

# --- pacstrap, with retries --------------------------------------------------
# Same classes and wording as the graphical installer (ec-setup-helper stages.rs).
classify_pacstrap() {
  local log="$1"
  if grep -qiE 'no space left on device|not enough free disk space|too full' "$log"; then
    echo disk
  elif grep -qiE 'pgp signature|signature from|is unknown trust|is marginal trust|key is disabled|required key missing|keyring is not writable|could not be looked up remotely|invalid or corrupted database' "$log"; then
    echo signature
  elif grep -qiE 'failed retrieving file|operation too slow|operation timed out|connection timed out|could not resolve|failed to connect|connection reset|connection refused|ssl connect error|failed to synchronize|invalid or corrupted package \(checksum\)|unexpected error' "$log"; then
    echo network
  else
    echo other
  fi
}

# Move the first Server line to the end, so the mirror that just failed goes last.
rotate_mirrorlist() {
  local ml=/etc/pacman.d/mirrorlist
  [[ $(grep -c '^[[:space:]]*Server' "$ml" 2>/dev/null) -ge 2 ]] || return 0
  awk '/^[[:space:]]*Server/ && !done { first=$0; done=1; next } { print } END { if (done) print first }' \
    "$ml" >"$ml.new" && cat "$ml.new" >"$ml"
  rm -f "$ml.new"
}

pacstrap_failed() {
  local class="$1" msg
  case $class in
    network)   msg="Downloads stalled on every mirror — check the connection (a filtered network or captive portal can do this), then run this script again." ;;
    disk)      msg="The target disk ran out of space — pick a larger disk, then run this script again." ;;
    signature) msg="Package signature check failed — check that the clock is right and the network is not altering downloads (captive portal, proxy), then run this script again." ;;
    *)         msg="Installing packages failed — read the pacman errors above, then run this script again." ;;
  esac
  printf '\n!! %s\n' "$msg" >&2
  unmount_target || true
  cat >&2 <<'NEXT'

   Nothing is left mounted. To try again:   /root/install-eclipseos.sh
   (it will unmount /mnt first if anything is still there). Packages already
   downloaded are kept for retries within one run, not across runs.
NEXT
  exit 1
}

# Downloaded packages stay in the target's pacman cache, so a retry fetches only
# what is missing. Only a network-class failure is retried, three attempts in
# all: a bad signature or a full disk would fail the same way every time.
ERRLOG="$(mktemp)"
attempt=1
while :; do
  : >"$ERRLOG"
  rc=0
  LC_ALL=C pacstrap -K -C "$PACCONF" /mnt "${PKGS[@]}" 2> >(tee "$ERRLOG" >&2) || rc=$?
  wait "$!" 2>/dev/null || true   # let tee flush before the log is read
  (( rc == 0 )) && break
  class="$(classify_pacstrap "$ERRLOG")"
  if [[ $class == network ]] && (( attempt < 3 )); then
    attempt=$((attempt + 1))
    say "downloads failed; retrying on a different mirror (attempt $attempt of 3)"
    stop_target_daemons
    rotate_mirrorlist
    sleep $((5 * (attempt - 1)))
    continue
  fi
  pacstrap_failed "$class"
done
rm -f "$ERRLOG"
# The baked db is re-signed at bake time, so it differs from the published one
# but looks newer; left in place, the first `pacman -Sy` keeps it and fetches the
# published signature (a bad signature). Let the first sync fetch both.
rm -f /mnt/var/lib/pacman/sync/eclipseos.db /mnt/var/lib/pacman/sync/eclipseos.db.sig
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
  # NetworkManager and iwd both start on their own, and when NM wins the race it
  # logs "IWD device named wlan0 is not a Wifi device", parks the device and
  # never autoconnects -- the laptop boots with no network until someone picks
  # the network by hand. Order NM after iwd and pull iwd in with it.
  install -Dm0644 /dev/stdin "/mnt/etc/systemd/system/NetworkManager.service.d/10-iwd-first.conf" <<'NMORDER'
[Unit]
Wants=iwd.service
After=iwd.service
NMORDER
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

NVCMD=""
if (( NVIDIA_OPEN )); then
  NVCMD=" nvidia-drm.modeset=1 nvidia-drm.fbdev=1"
  # The DKMS hook ran during pacstrap. A kernel without the module boots to a
  # black screen, so refuse here rather than after the reboot.
  for k in /mnt/usr/lib/modules/*/; do
    [[ -f ${k}vmlinuz ]] || continue
    compgen -G "${k}updates/dkms/nvidia.ko*" >/dev/null ||
      die "the NVIDIA kernel module did not build for $(basename "$k"); not continuing"
  done
  # D-01 §2.3: modules load in early userspace; the stock hooks (kms too) stay,
  # since `modconf` carries nvidia-utils' nouveau blacklist into the initramfs.
  install -Dm0644 /dev/stdin /mnt/etc/mkinitcpio.conf.d/eclipseos-nvidia.conf <<'MKINIT'
MODULES+=(nvidia nvidia_modeset nvidia_uvm nvidia_drm)
MKINIT
fi

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
    cmdline: root=UUID=$ROOT_UUID rw amd_pstate=active$NVCMD
    $UCODE
    module_path: boot():/initramfs-linux.img

/EclipseOS (fallback initramfs)
    protocol: linux
    path: boot():/vmlinuz-linux
    cmdline: root=UUID=$ROOT_UUID rw$NVCMD
    module_path: boot():/initramfs-linux-fallback.img

/EclipseOS (LTS kernel)
    protocol: linux
    path: boot():/vmlinuz-linux-lts
    cmdline: root=UUID=$ROOT_UUID rw amd_pstate=active$NVCMD
    $UCODE
    module_path: boot():/initramfs-linux-lts.img
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

say "set a password for root"
arch-chroot /mnt passwd
say "set a password for $USERNAME"
arch-chroot /mnt passwd "$USERNAME"

# --- remote access --------------------------------------------------------------
# Opt in, default no. openssh is in the package set; sshd stays off unless asked
# for, and then takes keys only. A machine with no working screen can otherwise
# only be debugged from a photograph of it.
read -rp $'\nenable ssh for remote debugging? [y/N] ' SSH
if [[ $SSH == [yY]* ]]; then
  read -rp "paste one public key for $USERNAME (ssh-ed25519 AAAA...): " SSHKEY
  [[ $SSHKEY == ssh-* || $SSHKEY == ecdsa-* || $SSHKEY == sk-* ]] || die "that does not look like a public key"
  install -d -m0700 "/mnt/home/$USERNAME/.ssh"
  printf '%s\n' "$SSHKEY" >"/mnt/home/$USERNAME/.ssh/authorized_keys"
  chmod 0600 "/mnt/home/$USERNAME/.ssh/authorized_keys"
  arch-chroot /mnt chown -R "$USERNAME:" "/home/$USERNAME/.ssh"
  install -Dm0644 /dev/stdin /mnt/etc/ssh/sshd_config.d/10-eclipseos.conf <<'SSHD'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
SSHD
  arch-chroot /mnt systemctl enable sshd
fi

umount -R /mnt
say "done. reboot, pick Abyss at the greeter."
