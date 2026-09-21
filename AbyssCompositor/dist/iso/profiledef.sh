#!/usr/bin/env bash
# shellcheck disable=SC2034

iso_name="eclipseos"
iso_label="ECLIPSEOS_$(date --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y%m)"
iso_publisher="EclipseOS <https://github.com/chasebrowndev/EclipseOS>"
iso_application="EclipseOS Live/Install Medium"
iso_version="$(date --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y.%m.%d)"
install_dir="eclipseos"
buildmodes=('iso')
# GRUB rather than releng's systemd-boot. A Framework 13 took the stick from
# the boot menu and then reported "booting from <device> failed" against the
# systemd-boot image, with Secure Boot out of the picture; GRUB's stub is the
# more forgiving of the two across firmware. mkarchiso rejects both bootmodes
# together -- they each own /EFI/BOOT/BOOTx64.EFI -- so this is a swap, not an
# addition. Revisit if the medium ever needs to boot Secure Boot signed.
bootmodes=('bios.syslinux'
           'uefi.grub')
pacman_conf="pacman.conf"
airootfs_image_type="squashfs"
airootfs_image_tool_options=('-comp' 'xz' '-Xbcj' 'x86,arm64' '-b' '1M' '-Xdict-size' '1M')
bootstrap_tarball_compression=('zstd' '-c' '-T0' '--auto-threads=logical' '--long' '-19')
file_permissions=(
  ["/etc/shadow"]="0:0:400"
  ["/root"]="0:0:750"
  ["/root/.gnupg"]="0:0:700"
  ["/usr/local/bin/Installation_guide"]="0:0:755"
  ["/usr/local/bin/livecd-sound"]="0:0:755"
)
file_permissions+=(
  ["/root/install-eclipseos.sh"]="0:0:755"
  ["/root/eclipseos-postinstall.sh"]="0:0:755"
  ["/root/eclipseos-packages.txt"]="0:0:644"
)
