<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# D-03 — Installation media and installer

Status: **written 2026-09-18**, profile built but never run through
`mkarchiso`. Depends on D-01 (written) and D-02 (written, reduced).

Governing specs: Tier 6 D-03. Constrained by D-01 (session and seat model),
D-02 §3 (repository trust), F-05 (licensing).

The medium is a UEFI-only live/install ISO for one machine — a Framework 13
AMD — built on the machine that builds the packages. Like D-02, it is written
for an audience of one, and the places where that shows are named rather than
hidden.

## 1. Derivation

`dist/iso/` is a copy of upstream archiso's `releng` profile with a rebrand and
two removals. Starting from `releng` rather than `baseline` is deliberate: the
live networking (iwd + `systemd-networkd` + `systemd-resolved`), the `.zlogin`
boot flow, the syslinux/GRUB menus and the mkinitcpio archiso hooks are
all things this project has no reason to reimplement and every reason to keep
matching upstream.

`profiledef.sh` differs only in identity and permissions:

```
iso_name="eclipseos"        install_dir="eclipseos"
iso_label="ECLIPSEOS_YYYYMM"
iso_publisher / iso_application  rebranded
file_permissions:  - choose-mirror, - automated_script
                   + /root/install-eclipseos.sh        0:0:755
                   + /root/eclipseos-packages.txt      0:0:644
```

**Removed from releng:**

- **The `choose-mirror` machinery** — the binary, the script, its unit and the
  wants-symlink, and the `automated_script` hook that ran it. It exists to
  rewrite `/etc/pacman.d/mirrorlist` from a kernel cmdline parameter for people
  installing from a mirror of their choosing over a network they do not control.
  Nothing here needs that, and leaving it in means a boot-time unit editing the
  one file the build-time `[eclipseos]` entry sits beside. `grep -rn
  choose-mirror dist/iso` is clean, and is the check to re-run after any
  upstream resync.
- **`pacman-init.service.d/`** — the drop-in, not the unit. `pacman-init`
  itself stays: the live medium still needs `pacman-key --init` and
  `--populate archlinux` before it can install anything. The drop-in was the
  `choose-mirror` ordering dependency, and with the mirror chooser gone it
  ordered against a unit that no longer exists.

Everything else under `airootfs/` is releng's, unmodified, plus the two new
`/root/` files.

## 2. Package selection

`packages.x86_64` is written for exactly one laptop (D-01 §2). The
Framework 13 AMD choices and why:

| Package | Why, and what it replaces |
|---|---|
| `amd-ucode` | Installed when `/proc/cpuinfo` says AMD; `intel-ucode` when Intel; neither in a VM. The Limine entry adds it as a `module_path` ahead of the initramfs. Intel is **untested** until the second machine is run. |
| `linux-firmware-amdgpu` | Split out of `linux-firmware` upstream; without it amdgpu does not come up on this generation. Named explicitly rather than relied on as a transitive pull. |
| `vulkan-radeon` + `mesa` + `libva-mesa-driver` | The RADV path. `amdvlk` is not installed — two Vulkan ICDs on one system is a loader-ordering problem, not a choice worth offering. |
| Detected GPUs (sysfs) | AMD as above; Intel adds `vulkan-intel` and `intel-media-driver`; NVIDIA uses the open-source nouveau/NVK stack (`vulkan-nouveau`), no proprietary or DKMS module. Intel and NVIDIA are detected and installed but **untested**. |
| *(no DKMS anything)* | amdgpu is in-tree. There is no out-of-tree module, so there is no `dkms`, no headers package, and **no kernel-upgrade step that can fail after reboot**. This is the single biggest reason the target hardware is AMD; it is a property of the install, not a preference. |

Beyond that: disk tooling the installer calls (`gptfdisk`, `dosfstools`,
`e2fsprogs`, `arch-install-scripts`), the D-Bus services the bar consumes
(`networkmanager`, `bluez`, `upower`, `pipewire`/`wireplumber`, `polkit` —
D-01 §1), the greeter set (`greetd`, `greetd-regreet`, `cage`, `foot`), fonts,
and `eclipseos-meta`, which pulls `abyss`, the desktop and `policyd`.

``noto-fonts-cjk` is on the live medium only (the wizard shows language names in their own script); the helper adds it to the target only for a ja/zh/ko locale.

`archinstall` is on the medium too, as the guided alternative (§4).

### 2.1 One list, two jobs

`airootfs/root/eclipseos-packages.txt` is `packages.x86_64` minus the
archiso-only entries (`mkinitcpio-archiso`, `mkinitcpio-nfs-utils`, `syslinux`,
`edk2-shell`, `memtest86+-efi`, `archinstall`). The installer pacstraps that
file. The property worth having is that **what you booted is what you install**:
if the live session drives the panel, the installed system has the same set of
packages driving it, and a "works on the ISO, dead after reboot" gap has one
fewer place to live.

## 3. Repository trust at build time

`dist/iso/pacman.conf` is stock but for one appended repository:

```
[eclipseos]
SigLevel = Optional TrustAll
Server = file:///home/chase/.local/share/eclipseos/repo/x86_64
```

This is the build-time half of the split described in **D-02 §3**, and the
disagreement with the installed system is the design, not an oversight:

- **Here**, `mkarchiso` reads packages this machine built and signed minutes
  earlier, off its own filesystem. The filesystem is the trust boundary.
  Requiring signatures would mean seeding the packaging key into `mkarchiso`'s
  throwaway keyring to verify a claim the path already establishes.
- **On the installed system**, `/etc/pacman.d/eclipseos.conf` (shipped by
  `eclipseos-meta`) points at `http://chase-pc:8088/x86_64` with
  `Required DatabaseRequired`, because there the packages cross a network.

`TrustAll` is therefore scoped to a local path on the build machine and never
reaches the target. The absolute path in it is the one machine-specific string
in the profile; a second build host edits this line.

## 4. The installer

`airootfs/root/install-eclipseos.sh` — about 115 lines, root-only, UEFI-only,
run by hand from the live root shell. Flow:

1. Refuse unless `EUID == 0` and `/sys/firmware/efi` exists.
2. `lsblk` the disks; prompt for the target; **require the disk path typed a
   second time** before anything destructive. Prompt for hostname and username.
3. `sgdisk --zap-all`, then GPT: 1 GiB `ef00` ESP + rest `8304` root.
   `partprobe` and `udevadm settle` before touching the new nodes.
4. `mkfs.fat -F32` / `mkfs.ext4`, mount at `/mnt` and `/mnt/boot`.
   No LUKS, no btrfs subvolumes, no swap — see §6.
5. `pacstrap -K /mnt` from `eclipseos-packages.txt`; `genfstab -U`.
6. Locale (`en_US.UTF-8`), timezone, hostname, `hwclock --systohc`.
7. Append `Include = /etc/pacman.d/eclipseos.conf` to `/mnt/etc/pacman.conf`
   if absent (D-02 §5). The *drop-in* ships in `eclipseos-meta`; the `Include`
   line is added here so that no package edits a file it does not own.
8. In `arch-chroot`: `mkinitcpio -P`; then Limine's stub is copied to
   the ESP's removable fallback path, with a pacman hook to keep it current,
   `useradd -m -G wheel`, a `%wheel` sudoers drop-in at 0440,
   `systemctl enable greetd NetworkManager iwd bluetooth systemd-timesyncd`.
9. A Limine config with `root=UUID=… rw amd_pstate=active`.
   `amd_pstate=active` is on from first boot rather than discovered later.
10. `passwd` for root and the new user, `umount -R /mnt`.

*(amended DA-04, 2026-09-24)* **This script is the fallback installer, not the
primary one.** The primary installer is `eclipse-setup --install`, a graphical
program in the live session (D-07), which asks these questions and many more
(language, network, profile, mode, components, applications, appearance,
displays, agents) and drives the same steps through the D-07 §6 helper. The
pacstrapped list becomes the D-07 §2.2 setup floor, `eclipseos-base`, installed
from the baked on-medium `[eclipseos]` repository (already the case for the
EclipseOS packages; their official-repo dependencies still need a mirror, see
D-07 §2.2). Specified, not built:
the script is what exists today, and the list is still `eclipseos-meta`.

### 4.1 Why this and not an `archinstall` JSON

`archinstall` supports an unattended config file, which is the obvious answer
and was rejected for a concrete reason: **`archinstall` is not installed on the
build machine, so its JSON schema could not be validated locally.** Shipping an
unverifiable config that is consumed by a tool whose schema moves between
releases is worse than shipping forty lines of `pacstrap` that can be read in
full. `bash -n` passes; it has never been executed end to end, and that is the
next thing that happens to it.

`archinstall` remains on the medium. Anyone who wants the guided installer has
it; this script is the one that produces *this* layout without questions.

### 4.2 No group management

The chroot block creates the user with `-G wheel` and nothing else. No `video`,
no `input`, no `render`, and specifically **no `seat` group — there is no such
group on Arch.** Per **D-01 §5**, a logind session on a seat is the entire
requirement: logind sets the ACL on the DRM and input nodes for whoever owns
the active session, and `seatd`/libseat consume that. Adding legacy groups here
would grant device access *outside* the session — ambient authority, granted at
install time, to a user who might not be the one sitting at the machine. The
absence of those lines is load-bearing; do not "fix" it when a device looks
inaccessible.

## 5. greetd on the live medium

*(rewritten with D-07 §2.1, 2026-09-24. Before this, the medium had only a root
shell on tty1 and greetd was not enabled.)*

The medium boots to a session, not a prompt. `airootfs/` carries:

- **`liveuser`**, made at boot by `sysusers.d/liveuser.conf` (uid 1000, no
  groups, no password, no sudoers or wheel entry; D-01 §5). `sysusers` rather
  than a copy of `/etc/group`, which would replace the base file wholesale.
- **`/etc/eclipse/greetd/config.toml`**, the shipped greetd config plus an
  `initial_session` running `abyss-session` as `liveuser`. This is the one
  deliberate second copy of an `eclipseos-meta` file: the live medium needs the
  initial session and the installed system must not have it. `display-manager`
  and `default.target` link greetd and `graphical.target`.
- **`/etc/systemd/user/eclipse-setup-live.service`**, wanted by
  `abyss-session.target` and conditioned on `/run/archiso`. It starts the
  installer. It is the medium's own unit, not a key in any `abyss.kdl`.
- **`/etc/polkit-1/rules.d/50-eclipse-live-install.rules`**, allowing
  `org.eclipse.install.apply` for a local, active `liveuser` only.
- **`kernel.yama.ptrace_scope = 1`**, so a same-uid client cannot trace the
  compositor that draws the erase confirmation (D-07 §6).
- **No sshd, no mDNS listener.** `sshd` is not enabled and its `PermitRootLogin`
  drop-in is gone; nothing on the medium listens to the network.
- **NetworkManager** owns networking (D-07 §4 step 3), with iwd as its wifi
  backend so `iwctl` and the fallback script's saved-network carry-over work.

The root shell moves to **tty2** and stays: root has an empty password, reachable
only from the local console now that sshd is gone. A session that will not start
on some hardware must not strand the user (D-07 §8), and `/root/install-eclipseos.sh`
runs from there.

## 6. Build

```
mkarchiso -v -w /var/tmp/eclipseos-work -o /var/tmp/eclipseos-out dist/iso
```

Root is required (loop mounts and `mkinitcpio` in a chroot), via `~/bin/ksudo`.
The work directory is under `/var/tmp` rather than `/tmp` because a tmpfs
`/tmp` cannot hold the extracted airootfs. `dist/repo/build-repo.sh` must have
run first — `mkarchiso` resolves `eclipseos-meta` out of the `file://` repo of
§3, and an empty repo fails as a missing package rather than a missing repo.

## 7. What this does not do

No LUKS (top known gap, deferred to D-04), no swap partition (zram only), no btrfs snapshots, no secure boot enrolment, no unattended
mode, no second target machine. Each is a real gap and each is deferred:
disk encryption and rollback belong with D-04 (update strategy), which is also
where "what happens when an upgrade breaks the session" is answered. D-03's job
ends when a machine boots to the greeter with a signed EclipseOS installed.
