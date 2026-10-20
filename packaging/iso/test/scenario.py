#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""End-to-end install scenario for the EclipseOS live ISO in headless QEMU (python3 stdlib only).

    fresh boot -> (no keystroke reaches the welcome screen) -> Space -> every ec-setup step
    -> trusted erase confirmation -> install -> reboot to disk -> greeter login -> session
    -> health checks over SSH (optionally after N more reboots) -> <work>/result.json

Built on vm.py (QMP client, input, screendump, waits, pid-safe stop); nothing is duplicated.
It never builds an ISO and never signals a process it did not spawn. See README.md.

What it cannot do without OCR: read the screen. Progress is judged by "the screen changed
and settled" plus facts read out of the guest over two side channels:
  * the live session's tty2 root shell (Ctrl+Alt+F2), whose output is sent to the serial
    port (/dev/ttyS0) and read back from <work>/serial.log between markers;
  * SSH to the installed system through a SLIRP hostfwd.
ec-setup has no remote-access step, so the SSH key is planted into the finished install from
the live shell before the first reboot (key only, like eclipseos-postinstall.sh's opt-in).
"""
import argparse
import http.server
import json
import os
import re
import shlex
import shutil
import socket
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import imgtools as it  # noqa: E402
import vm as vmmod  # noqa: E402
from vm import HarnessError, QMPError, VM, log  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FLOOR_LIST = os.path.join(HERE, "..", "airootfs", "root", "eclipseos-packages.txt")

USER = "tester"
PASSWORD = "eclipse-test-pw"  # throwaway; appears in argv of `type` and, never, in logs
HOSTNAME = "eclipse"  # ec-setup's default; the identity step is left on it
SSID = "eclipse-test"
WIFI_PSK = "eclipse-test-psk-1"
GUEST_FROM_SLIRP = "10.0.2.2"  # the host, as seen from the guest

PROFILES = ("minimal", "standard", "full", "agentic")
# installer/crates/ec-setup/src/data.rs `selectable`: only Standard could be chosen when this was
# written (the other cards are inert); selectable_profiles() reads the live answer.
SELECTABLE_FALLBACK = {"standard"}
DATA_RS = os.path.join(HERE, "..", "..", "..", "installer", "crates", "ec-setup", "src", "data.rs")


def selectable_profiles(path=DATA_RS):
    """Profiles ec-setup lets the user pick, read from `data::selectable` so this follows the
    installer; the fallback is today's answer when the source is not in the checkout."""
    try:
        with open(path) as f:
            body = f.read().split("pub fn selectable(p: Profile) -> bool {")[1].split("\n}")[0]
    except (OSError, IndexError):
        return set(SELECTABLE_FALLBACK)
    found = {m.lower() for m in re.findall(r"Profile::(\w+)", body)}
    return found or set(SELECTABLE_FALLBACK)

# The wizard's screens in order (installer/crates/ec-setup/src/model.rs `Step::ALL`).
SCREENS = ("welcome", "language", "keyboard", "timezone", "network", "disk", "identity",
           "profile", "mode", "layout", "components", "appearance", "apps", "review", "install")

# Packages a profile's catalog ids resolve to (installer/crates/ec-setup/src/choices.rs for_profile
# + ec-setup-helper catalog.rs). The floor list is read from the medium's package list.
PROFILE_COMPONENTS = {
    "minimal": ["eclipseos-launcher"],
    "standard": ["eclipseos-hyperion", "eclipseos-launcher", "eclipseos-toasts", "eclipseos-center"],
}
PROFILE_COMPONENTS["full"] = PROFILE_COMPONENTS["standard"] + [
    "firefox", "micro", "thunar", "mpv", "imv", "zathura", "zathura-pdf-mupdf"]
PROFILE_COMPONENTS["agentic"] = PROFILE_COMPONENTS["standard"]
AGENTS_PACKAGE = "eclipseos-agents"

# User units each package ships in abyss-session.target.wants (packaging/pkg/eclipseos/PKGBUILD).
PACKAGE_USER_UNITS = {
    "eclipseos-hyperion": ["ec-hyperion-bar.service"],
    "eclipseos-toasts": ["ec-toasts.service"],
    "eclipseos-wallpaper": ["ec-wallpaper.service"],
    "eclipseos-desktop": ["ec-screensaver.service", "ec-pairing.service"],
    "eclipseos-agents": ["ec-policyd.service", "ec-brokerd.service", "ec-agentd.service",
                         "ec-inferenced.service"],
    "eclipseos-fog": ["ec-fogd.socket"],
}
SYSTEM_ACTIVE = ["greetd", "NetworkManager"]  # catalog.rs FLOOR_UNITS that must be running
SYSTEM_ENABLED = ["bluetooth", "systemd-timesyncd", "fstrim.timer"]  # may need hardware to run
JOURNAL_IGNORE = ()  # regexes of known-benign ec-* error lines; empty until one is justified


class ScenarioStop(Exception):
    """--stop-at reached: a pass, not a failure."""


class ScenarioAbort(HarnessError):
    """A step failed in a way that makes the rest meaningless."""


# ================================================================ pure logic

def parse_res(s):
    m = re.fullmatch(r"(\d{3,5})x(\d{3,5})", s or "")
    if not m:
        raise HarnessError("--res must look like 1280x800, got %r" % s)
    return int(m.group(1)), int(m.group(2))


def build_parser():
    p = argparse.ArgumentParser(prog="scenario.py", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--iso", required=True, help="path to the live ISO")
    p.add_argument("--work", default=vmmod.default_work(), help="work dir (outside the repo)")
    p.add_argument("--res", default="1280x800", help="virtio-gpu resolution WxH")
    p.add_argument("--profile", choices=PROFILES, default="standard")
    p.add_argument("--disk", choices=["virtio", "nvme"], default="virtio")
    p.add_argument("--disk-size", default="20G")
    p.add_argument("--second-disk", metavar="SIZE", help="attach a blank second disk (left untouched)")
    p.add_argument("--no-net", action="store_true", help="no network device: expects the install to stop at its first download")
    p.add_argument("--bios", action="store_true", help="boot SeaBIOS: expect a clean UEFI-required refusal")
    p.add_argument("--wifi", action="store_true", help="join a simulated WPA2 AP (mac80211_hwsim + hostapd) in the wizard")
    p.add_argument("--stop-at", choices=SCREENS, default=None, metavar="SCREEN",
                   help="stop successfully once this wizard screen is showing (no install)")
    p.add_argument("--force-profile", action="store_true",
                   help="run a profile even if this checkout's data.rs says it is not selectable (ISO built from another branch)")
    p.add_argument("--boot-entry", choices=("default", "lts"), default="default",
                   help="reboot into the installed system through this Limine entry (lts: pick it from the menu)")
    p.add_argument("--vt-test", action="store_true", help="after login: tty3 round trip, then check input still reaches the session")
    p.add_argument("--polkit-workaround", action="store_true",
                   help="plant a live polkit rule letting liveuser use NetworkManager (the ISO ships none; join otherwise fails)")
    p.add_argument("--keep", action="store_true", help="leave the VM running afterwards")
    p.add_argument("--reboot-count", type=int, default=0, metavar="N", help="extra reboots with a recheck after each")
    p.add_argument("--mem", type=int, default=4096)
    p.add_argument("--smp", type=int, default=2)
    p.add_argument("--tcg", action="store_true", help="force TCG (timeouts scale x4 without KVM)")
    p.add_argument("--kernels", default="linux,linux-lts", help="kernels Limine must have entries for")
    p.add_argument("--packages-file", default=FLOOR_LIST, help="package floor list the install must contain")
    p.add_argument("--disk-by-id", help="override the expected /dev/disk/by-id name")
    return p


def parse_args(argv=None):
    a = build_parser().parse_args(argv)
    parse_res(a.res)
    if a.reboot_count < 0:
        raise HarnessError("--reboot-count must be >= 0")
    if not re.fullmatch(r"\d+[KMGTkmgt]?", a.disk_size):
        raise HarnessError("--disk-size must look like 20G")
    if a.second_disk and not re.fullmatch(r"\d+[KMGTkmgt]?", a.second_disk):
        raise HarnessError("--second-disk must look like 8G")
    if a.wifi and a.no_net:
        raise HarnessError("--wifi needs the ethernet uplink (hostapd/dnsmasq come from the mirror); drop --no-net")
    if a.wifi and a.bios:
        raise HarnessError("--wifi and --bios do not combine")
    if a.bios and a.no_net:
        raise HarnessError("--bios and --no-net do not combine")
    return a


def expected_by_id(bus, serial="eclipse0"):
    """The /dev/disk/by-id name udev gives the harness's disks (see vm.build_qemu_args)."""
    return ("nvme-QEMU_NVMe_Ctrl_%s" if bus == "nvme" else "virtio-%s") % serial


def preferred_by_id(names):
    """ec-setup-helper's pick among a disk's by-id aliases (disks.rs `preferred`)."""
    names = [n for n in names if n]
    if not names:
        return None
    return min(names, key=lambda n: (n.startswith("wwn-") or "eui." in n, len(n), n))


def disk_ids_from_listing(listing, serial):
    """Whole-disk by-id names (no -partN) mentioning `serial`, from `ls /dev/disk/by-id`."""
    return [n for n in listing.split() if serial in n and "-part" not in n]


def step_plan(ctx):
    """The keystroke script for every screen except the last, as a list of dicts:
    {screen, prep: [...], go: [...]}. `prep` changes the screen without leaving it (typing,
    selecting); `go` leaves it and must change the screen. Actions are tuples:
    ("key", combo) ("type", text) ("click", fx, fy) ("sleep", secs) ("wait", secs-of-stable).
    ctx: by_id, wifi (join the AP in the wizard)."""
    ret = [("key", "ret")]
    plan = []

    def add(screen, prep=(), go=ret, frac=0.01):
        plan.append({"screen": screen, "prep": list(prep), "go": list(go), "frac": frac})

    add("welcome", go=[("key", "spc")], frac=0.05)  # the greeting cycles languages, so only a big change counts
    add("language")
    # Both keyboard-step fields swallow Enter, so click empty ground left of the column first.
    add("keyboard", prep=[("click",) + CLICK_BLANK])
    add("timezone")  # the search box is focused; its Enter advances
    if ctx.get("wifi"):
        add("network", prep=[("click",) + CLICK_WIFI_LINK, ("sleep", 3), ("click",) + CLICK_WIFI_SSID, ("sleep", 2), ("click",) + CLICK_WIFI_PSK,
                             ("sleep", 1), ("type", WIFI_PSK), ("key", "ret"), ("sleep", 10), ("wait", 6), ("shot", "network-joined")])
    else:
        add("network")
    add("disk", prep=[("sleep", 2), ("key", "down")])
    add("identity", prep=[("sleep", 1), ("type", USER), ("key", "tab"), ("type", PASSWORD),
                          ("key", "tab"), ("type", PASSWORD)])
    card = PROFILE_CARDS.get(ctx.get("profile", "standard"))
    add("profile", prep=[("click",) + card, ("sleep", 1)] if card else [])  # Standard is preselected
    for s in ("mode", "layout", "components", "appearance", "apps"):
        add(s)
    add("review", prep=[("sleep", 1), ("type", ctx["by_id"])])
    return plan


# Fractions of the screen, tuned against real screenshots (see README, "Calibration").
CLICK_BLANK = (0.04, 0.50)  # empty ground left of the centred content column
CLICK_WIFI_LINK = (0.25, 0.246)  # "Connect to Wi-Fi" under the Network lead (recalibrated 2026-10-19)
CLICK_WIFI_SSID = (0.5, 0.281)  # the (only) SSID row: it must be selected before the passphrase field exists
CLICK_WIFI_PSK = (0.44, 0.409)  # passphrase field once the SSID row is shown (SSID preselected)
# Profile cards stacked in the order of data.rs CARDS; Standard is the default and needs no click.
PROFILE_CARDS = {"agentic": (0.5, 0.435)}  # 2026-10-19: only Standard (preselected) and Agentic are offered
CLICK_RESTART_BR = (57, 41)  # done screen Restart: centre, px left of the column edge and up from the bottom (same at 1280x800 and 1920x1080)
CLICK_PASSWORD = (0.50, 0.58)  # greeter password field, if it is not focused already


def screens_covered(plan):
    return [p["screen"] for p in plan]


def expected_packages(profile, floor, agents=None):
    """Packages that must be installed for `profile` (floor list + catalog + agents add-on)."""
    out = list(floor)
    for p in PROFILE_COMPONENTS[profile]:
        if p not in out:
            out.append(p)
    if agents if agents is not None else profile == "agentic":
        out.append(AGENTS_PACKAGE)
    return out


def read_floor(path):
    out = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#"):
                out.append(line)
    return out


def expected_user_units(installed):
    """User units that must be active, from the packages actually installed."""
    out = []
    for pkg, units in PACKAGE_USER_UNITS.items():
        if pkg in installed:
            out += [u for u in units if u not in out]
    return out


def parse_kv(text):
    """`name=value` lines -> dict (last wins); other lines ignored."""
    d = {}
    for line in text.splitlines():
        m = re.fullmatch(r"([A-Za-z0-9_.@:-]+)=(.*)", line.strip())
        if m:
            d[m.group(1)] = m.group(2).strip()
    return d


def eval_active(kv, expected, want="active"):
    bad = ["%s=%s" % (u, kv.get(u, "missing")) for u in expected if kv.get(u) != want]
    return (not bad, "all %d %s" % (len(expected), want) if not bad else ", ".join(bad))


def journal_ec_errors(text, ignore=JOURNAL_IGNORE):
    """Error-priority journal lines that name an ec-* component."""
    out = []
    for line in text.splitlines():
        if re.search(r"\bec-[a-z]", line) and not any(re.search(i, line) for i in ignore):
            out.append(line.strip())
    return out


def limine_kernels(text):
    """Kernel names with a boot entry in a limine.conf (vmlinuz-linux, vmlinuz-linux-lts, ...)."""
    return sorted(set(re.findall(r"vmlinuz-([A-Za-z0-9._-]+)", text)))


def eval_limine(text, kernels):
    have = limine_kernels(text)
    missing = [k for k in kernels if k not in have]
    return (not missing, "entries for %s" % (", ".join(have) or "none") if not missing
            else "no entry for %s (found: %s)" % (", ".join(missing), ", ".join(have) or "none"))


def session_ready(loginctl_text, user):
    """True if `loginctl list-sessions --no-legend` shows `user` on a seat."""
    for line in loginctl_text.splitlines():
        f = line.split()
        if len(f) >= 4 and f[2] == user and f[3].startswith("seat"):
            return True
    return False


def parse_sections(text):
    """Split `@@S:name` marker sections (as the probe scripts print them) into a dict."""
    out, cur = {}, None
    for line in text.splitlines():
        line = line.rstrip("\r")
        m = re.fullmatch(r"@@S:(\S+)", line)
        if m:
            cur = m.group(1)
            out[cur] = []
        elif cur is not None:
            out[cur].append(line)
    return {k: "\n".join(v).strip() for k, v in out.items()}


def parse_marker(serial_text, tag):
    """(rc, output) of the shell command that wrapped itself in tag's markers, or None."""
    m = re.search(r"@@B:%s\r?\n(.*?)@@E:%s:(\d+)" % (re.escape(tag), re.escape(tag)),
                  serial_text, re.S)
    if not m:
        return None
    return int(m.group(2)), m.group(1).replace("\r\n", "\n")


def wrap_command(cmd, tag, tty="/dev/ttyS0"):
    """One typed line that runs `cmd` and reports (output, rc) on the serial port."""
    return ('{ %s; } >/tmp/o 2>&1; R=$?; { echo @@B:%s; cat /tmp/o; echo @@E:%s:$R; } >%s'
            % (cmd, tag, tag, tty))


# ---------------------------------------------------------------- guest-side scripts

def wifi_ap_script(ssid=SSID, psk=WIFI_PSK):
    """Run on the live session as root: two simulated radios, hostapd (WPA2-PSK) and a DHCP
    server on one of them, moved into a network namespace so iwd/NetworkManager keep the other
    as the station. Exit codes name the failing stage."""
    return """set -u
modprobe mac80211_hwsim radios=2 || exit 11
sleep 2
pacman -Sy --noconfirm hostapd dnsmasq >/tmp/ap-pacman.log 2>&1 || exit 12
ip netns add apns || exit 13
P=$(iw phy | awk '/^Wiphy/{print $2}' | sort | tail -1)  # phy1: iwd manages phy0 (wlan0) as the station
iw phy "$P" set netns name apns || { ip netns exec apns sleep 100000 & iw phy "$P" set netns $!; } || exit 14
IF=
for i in 1 2 3 4 5 6 7 8 9 10; do sleep 1; IF=$(ip netns exec apns iw dev | awk '/Interface/{print $2; exit}'); test -n "$IF" && break; done
test -n "$IF" || exit 15
ip netns exec apns ip addr add 192.168.77.1/24 dev "$IF"
ip netns exec apns ip link set "$IF" up
cat >/tmp/hostapd.conf <<EOF
interface=$IF
driver=nl80211
ssid=%(ssid)s
hw_mode=g
channel=6
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
wpa_passphrase=%(psk)s
EOF
ip netns exec apns hostapd -B -P /tmp/hostapd.pid /tmp/hostapd.conf >/tmp/hostapd.log 2>&1 || exit 16
ip netns exec apns dnsmasq --conf-file=/dev/null --interface="$IF" --bind-interfaces \\
  --except-interface=lo --dhcp-range=192.168.77.50,192.168.77.99,12h --dhcp-option=3 \\
  --pid-file=/tmp/dnsmasq.pid || exit 17
for i in 1 2 3 4 5 6 7 8 9 10 11 12; do nmcli device wifi rescan >/dev/null 2>&1; sleep 3; nmcli -t -f SSID device wifi list | grep -qx '%(ssid)s' && break; done
echo "@@S:ap"
echo up
echo "@@S:nm"
nmcli -t -f DEVICE,TYPE,STATE device status
nmcli -t -f SSID device wifi list
""" % {"ssid": ssid, "psk": psk}


def target_probe_script(by_id, pubkey, user=USER, ssid=SSID):
    """Run on the live session as root after the helper has exited: read facts out of the
    finished install and plant `pubkey` for SSH (key only, sshd enabled), then unmount."""
    return """set -u
ID=%(id)s
D=/dev/disk/by-id/$ID
echo "@@S:installlog"
tail -25 /run/eclipseos-install.log 2>&1 | cut -c1-240
echo "@@S:leftover"
findmnt -rn -o TARGET,SOURCE | grep -a '^/mnt'
for p in /proc/[0-9]*; do for l in cwd root; do t=$(readlink "$p/$l" 2>/dev/null); case "$t" in /mnt*) echo "holder pid=${p#/proc/} comm=$(cat $p/comm 2>/dev/null) $l=$t $(tr '\\0' ' ' <$p/cmdline | cut -c1-80)";; esac; done; done
for p in /proc/[0-9]*; do for f in $p/fd/*; do t=$(readlink "$f" 2>/dev/null); case "$t" in /mnt/*) echo "openfile pid=${p#/proc/} $(cat $p/comm) $t";; esac; done; done 2>/dev/null | sort -u | head -20
echo "nsmounts: $(grep -l -a ' /mnt' /proc/[0-9]*/mountinfo 2>/dev/null | tr '\\n' ' ')"
echo "procs: $(ps -eo pid,comm | grep -a -E 'gpg|dirmngr|keyboxd|pacman|chroot|agent' | tr '\\n' ';')"
echo "maps: $(grep -l -a '/mnt/' /proc/[0-9]*/maps 2>/dev/null | tr '\\n' ' ')"
for p in $(pgrep -x -a 'gpg-agent|dirmngr|keyboxd' | cut -d' ' -f1); do echo "agent pid=$p ppid=$(awk '/^PPid/{print $2}' /proc/$p/status) start=$(ps -o lstart= -p $p) cmd=$(tr '\\0' ' ' </proc/$p/cmdline) fds=$(ls -l /proc/$p/fd 2>/dev/null | awk '{print $NF}' | tr '\\n' ' ' | cut -c1-300)"; done
echo "livenm: $(ls -l /etc/NetworkManager/system-connections 2>&1 | tr '\\n' ';')"
nmcli -t -f NAME,TYPE,DEVICE connection show | sed 's/^/livecon: /'
echo "umount-try: $(umount /mnt 2>&1 | head -2)"
findmnt -rn -o TARGET,SOURCE | grep -a '^/mnt'
echo "@@S:parts"
lsblk -nrpo NAME,FSTYPE "$D"
ROOT=$(lsblk -nrpo NAME,FSTYPE "$D" | awk '$2=="ext4"{print $1; exit}')
ESP=$(lsblk -nrpo NAME,FSTYPE "$D" | awk '$2=="vfat"{print $1; exit}')
mkdir -p /mnt/t
mount "$ROOT" /mnt/t || { echo "@@S:error"; echo "root mount failed"; exit 1; }
test -n "$ESP" && mount "$ESP" /mnt/t/boot
echo "@@S:hostname"
cat /mnt/t/etc/hostname
echo "@@S:passwd"
grep '^%(user)s:' /mnt/t/etc/passwd
echo "@@S:limine"
cat /mnt/t/boot/limine.conf /mnt/t/boot/limine/limine.conf /mnt/t/boot/EFI/BOOT/limine.conf 2>/dev/null
echo "@@S:boot"
ls /mnt/t/boot /mnt/t/boot/EFI/BOOT
echo "@@S:packages"
pacman -r /mnt/t -b /mnt/t/var/lib/pacman -Qq
echo "@@S:nmconn"
grep -l 'ssid=%(ssid)s' /mnt/t/etc/NetworkManager/system-connections/* 2>/dev/null
echo "@@S:sshd"
test -x /mnt/t/usr/bin/sshd && echo present
H=$(awk -F: '$1=="%(user)s"{print $6}' /mnt/t/etc/passwd)
IDS=$(awk -F: '$1=="%(user)s"{print $3":"$4}' /mnt/t/etc/passwd)
install -d -m0700 "/mnt/t$H/.ssh"
echo '%(key)s' >"/mnt/t$H/.ssh/authorized_keys"
chmod 0600 "/mnt/t$H/.ssh/authorized_keys"
chown -R "$IDS" "/mnt/t$H/.ssh"
install -Dm0644 /dev/stdin /mnt/t/etc/ssh/sshd_config.d/10-eclipseos.conf <<EOF
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
EOF
systemctl --root=/mnt/t enable sshd
echo "@@S:end"
sync
umount -R /mnt/t
echo "@@S:umount"
echo $?
""" % {"id": by_id, "user": user, "key": pubkey.strip(), "ssid": ssid}


def health_script(units, user_id_expr="$(id -u)"):
    """Run over SSH as the test user: user-unit states as name=state lines."""
    lines = ["export XDG_RUNTIME_DIR=/run/user/%s" % user_id_expr,
             'export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus']
    for u in units:
        lines.append('echo "%s=$(systemctl --user is-active %s)"' % (u, u))
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------- result bookkeeping

MIN_DISK_GIB = 16  # installer/crates/ec-setup-helper/src/apply.rs MIN_DISK_BYTES


def disk_gib(size):
    """GiB of a qemu-img size like 20G, 512M, 8192K or a bare byte count."""
    m = re.fullmatch(r"(\d+)([KMGTkmgt]?)", size)
    mult = {"": 1, "k": 1 << 10, "m": 1 << 20, "g": 1 << 30, "t": 1 << 40}[m.group(2).lower()]
    return int(m.group(1)) * mult / float(1 << 30)


def bar_index(img):
    """0-based wizard step from the progress bar's white (current) segment, or None.
    The bar sits at the top of the centred 776 px column: 26.1 px pitch, current segment bright."""
    y = 40  # fixed px: the bar sits 40 px from the top at 1280x800 and 1920x1080 alike
    left = (img.w - 776) / 2.0
    run = None
    for x in range(int(left) - 4, int(left) + 776):
        i = 3 * (y * img.w + x)
        if min(img.data[i], img.data[i + 1], img.data[i + 2]) > 235:
            run = x
            break
    if run is None:
        return None
    n = round((run - left) / 26.1)
    return n if 0 <= n < 13 else None


def step_bar_target(screen):
    """The bar index a screen shows, or None for screens without a bar (welcome, install)."""
    if screen in ("welcome", "install"):
        return None
    return SCREENS.index(screen) - 1


class Results:
    def __init__(self, args):
        self.args = args
        self.checks = []
        self.started = time.time()
        self.notes = []

    def add(self, name, ok, detail=""):
        self.checks.append({"name": name, "ok": bool(ok), "detail": str(detail)[:2000]})
        log("%s %s %s" % ("PASS" if ok else "FAIL", name, str(detail)[:300]))
        return bool(ok)

    def skip(self, name, why):
        self.checks.append({"name": name, "ok": True, "skipped": True, "detail": why})
        log("SKIP %s %s" % (name, why))

    @property
    def passed(self):
        return bool(self.checks) and all(c["ok"] for c in self.checks)

    def to_json(self):
        a = self.args
        return {"pass": self.passed, "seconds": round(time.time() - self.started),
                "iso": os.path.abspath(a.iso), "profile": a.profile, "disk": a.disk,
                "res": a.res, "bios": a.bios, "wifi": a.wifi, "no_net": a.no_net,
                "reboot_count": a.reboot_count, "notes": self.notes, "checks": self.checks}

    def write(self, path):
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(self.to_json(), f, indent=1)
            f.write("\n")
        os.replace(tmp, path)


# ================================================================ host-side helpers

def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class ScriptServer:
    """Serves {name: text} on 127.0.0.1 so the guest can `curl 10.0.2.2:PORT/name | sh`
    instead of the harness typing long scripts at keyboard speed."""

    def __init__(self, files):
        self.files = files
        outer = self

        class H(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                body = outer.files.get(self.path.lstrip("/"))
                if body is None:
                    self.send_error(404)
                    return
                data = body.encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *a):
                pass

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


def make_ssh_key(directory):
    os.makedirs(directory, mode=0o700, exist_ok=True)
    key = os.path.join(directory, "id_ed25519")
    if not os.path.exists(key):
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "eclipse-scenario",
                        "-f", key], check=True)
    with open(key + ".pub") as f:
        return key, f.read().strip()


class Ssh:
    def __init__(self, port, key, user=USER):
        self.port, self.key, self.user = port, key, user

    def argv(self, remote):
        return ["ssh", "-p", str(self.port), "-i", self.key, "-o", "BatchMode=yes",
                "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=5", "-o", "IdentitiesOnly=yes",
                "%s@127.0.0.1" % self.user, remote]

    def run(self, cmd, timeout=60, sudo=False, stdin=None):
        """(rc, stdout+stderr). sudo feeds the throwaway password on stdin, never on argv."""
        data = stdin
        if sudo:
            cmd = "sudo -S -p '' sh -c %s" % shlex.quote(cmd)
            data = PASSWORD + "\n" + (stdin or "")
        try:
            r = subprocess.run(self.argv(cmd), input=data, capture_output=True, text=True,
                               timeout=timeout)
        except subprocess.TimeoutExpired:
            return 124, "timeout"
        return r.returncode, (r.stdout + r.stderr)

    def up(self):
        return self.run("true", timeout=15)[0] == 0

    def wait_up(self, timeout, interval=5):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if self.up():
                return True
            time.sleep(interval)
        return False

    def wait_down(self, timeout, interval=2):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if not self.up():
                return True
            time.sleep(interval)
        return False


# ================================================================ the guest driver

class Driver:
    def __init__(self, vm, scale, server=None):
        self.vm, self.scale, self.server = vm, scale, server
        self.n = 0
        self.size = None
        self.last = None
        self.serial_pos = 0
        self.tag = 0
        self.pace = 0.0  # extra seconds to let a slow display catch up after each step

    def t(self, secs):
        return int(secs * self.scale)

    # -- screens
    def shot(self, name):
        img = self.vm.screendump(out_png=os.path.join(self.vm.shots, "%02d-%s.png" % (self.n, name)))
        self.n += 1
        self.size = (img.w, img.h)
        self.last = img
        return img

    def settle(self, timeout=30, quiet=2.0):
        try:
            return self.vm.wait_stable(self.t(timeout), quiet=quiet, tolerance=12, max_frac=0.004)
        except HarnessError:
            return self.vm.screendump()  # animations never rest: take what is there

    def changed(self, base, timeout, max_frac=0.01):
        """Wait for the screen to differ from `base` by more than max_frac; returns the image or None."""
        try:
            return self.vm.wait_change(self.t(timeout), tolerance=16, max_frac=max_frac, baseline=base)
        except HarnessError:
            return None

    # -- input
    def do(self, actions):
        for a in actions:
            kind = a[0]
            if kind == "key":
                self.vm.key(a[1])
                time.sleep(0.3)
            elif kind == "type":
                self.vm.type_text(a[1], delay=0.15)
                time.sleep(0.3)
            elif kind == "click":
                w, h = self.size or self.vm.screen_size()
                self.vm.click(int(a[1] * w), int(a[2] * h), size=(w, h))
                time.sleep(0.5)
            elif kind == "clickbr":  # px offsets from the bottom-right of the 776 px content column
                w, h = self.size or self.vm.screen_size()
                self.vm.click(int((w + 776) / 2 - a[1]), int(h - a[2]), size=(w, h))
                time.sleep(0.5)
            elif kind == "shot":
                self.shot(a[1])
            elif kind == "sleep":
                time.sleep(a[1])
            elif kind == "wait":
                self.settle(timeout=90 if a[1] > 5 else 30, quiet=float(a[1]))
            else:
                raise HarnessError("unknown action %r" % (a,))

    def wait_bar(self, target, timeout):
        """Poll until the progress bar shows step `target`; returns the image or None."""
        end = time.monotonic() + self.t(timeout)
        while time.monotonic() < end:
            img = self.vm.screendump()
            if bar_index(img) == target:
                return img
            time.sleep(1)
        return None

    def advance(self, plan_item, next_name, timeout=40, retries=2):
        """Run one step's prep, then its go actions until the screen changes; shoot the result.
        Where the wizard shows a progress bar it is the truth (a slow display lags the keys)."""
        here, there = step_bar_target(plan_item["screen"]), step_bar_target(next_name)
        if here is not None and self.pace:
            self.wait_bar(here, 60)
            time.sleep(self.pace / 3)
        self.do(plan_item["prep"])
        base = self.vm.screendump()
        for attempt in range(retries + 1):
            self.do(plan_item["go"])
            if there is not None:
                img = self.wait_bar(there, timeout * (3 if self.pace else 1))
            else:
                img = self.changed(base, timeout, plan_item.get("frac", 0.01))
            if img is not None:
                time.sleep(self.pace / 3)
                self.settle()
                return self.shot(next_name)
            now = self.vm.screendump()
            dd = it.diff_images(base, now, 16) if now is not None else None
            if dd is not None and not it.matches(dd, plan_item.get("frac", 0.01)):
                # the guest was slower than one screendump: the step did advance, do not press again
                time.sleep(self.pace / 3)
                self.settle()
                return self.shot(next_name)
            log("step %s: screen did not change (attempt %d)" % (plan_item["screen"], attempt + 1))
        self.shot("stuck-" + plan_item["screen"])
        raise ScenarioAbort("screen did not leave %s" % plan_item["screen"])

    @staticmethod
    def is_prompt(img):
        """The compositor's erase dialog: it carries red warning text ("This cannot be undone") in the
        middle of the screen; the plain installing screen has no red there."""
        w, h = img.w, img.h
        n = 0
        for y in range(h // 2 - 250, h // 2 + 100, 2):
            for x in range(w // 2 - 260, w // 2 + 260, 2):
                i = 3 * (y * w + x)
                r, g, b = img.data[i], img.data[i + 1], img.data[i + 2]
                if r > 190 and g < 150 and b < 130:
                    n += 1
        return n > 150

    # -- serial side channel (live root shell on tty2)
    def serial_text(self):
        try:
            with open(self.vm.serial_log, errors="replace") as f:
                return f.read()
        except OSError:
            return ""

    def switch_vt(self, n):
        self.vm.key("ctrl+alt+f%d" % n)
        time.sleep(3)

    def login_root(self):
        for attempt in range(3):
            self.switch_vt(2)
            time.sleep(4 * attempt)  # a getty started by the switch itself needs a moment
            self.vm.key("ret")  # wake a blank console / reach a fresh prompt
            time.sleep(2)
            self.vm.type_text("root\n", delay=0.1)
            time.sleep(3)
            self.vm.key("ret")  # empty password on the live medium
            time.sleep(3)
            for _ in range(4):
                if self.sh("true", timeout=10, quiet=True) is not None:
                    return True
                time.sleep(3)
        self.shot("tty2-login-failed")
        return False

    def sh(self, cmd, timeout=60, quiet=False):
        """Run `cmd` in the root shell on tty2; (rc, output) via the serial port, or None."""
        self.tag += 1
        tag = "t%d" % self.tag
        if self.server is not None:
            # Fast typing garbles shifted characters under load, so type only a short fetch;
            # the wrapped command itself travels over HTTP.
            self.server.files[tag] = wrap_command(cmd, tag) + "\n"
            self.vm.type_text("curl -fsS http://%s:%d/%s|sh\n" % (GUEST_FROM_SLIRP, self.server.port, tag), delay=0.1)
        else:
            self.vm.type_text(wrap_command(cmd, tag) + "\n", delay=0.12)
        end = time.monotonic() + self.t(timeout)
        while time.monotonic() < end:
            got = parse_marker(self.serial_text(), tag)
            if got:
                return got
            time.sleep(1)
        if not quiet:
            log("shell command timed out: %s" % cmd[:80])
        return None

    def sh_script(self, name, text, timeout=120):
        """Run a multi-line script as root: fetched from the host over SLIRP if the guest has
        a network, else typed in through a heredoc."""
        if self.server is not None:
            self.server.files[name] = text
            return self.sh("curl -fsS http://%s:%d/%s | sh" % (GUEST_FROM_SLIRP, self.server.port, name),
                           timeout=timeout)
        return self.sh("cat >/tmp/%s <<'SCN_EOF'\n%s\nSCN_EOF\nsh /tmp/%s" % (name, text, name), timeout=timeout)


# ================================================================ the scenario

def vm_kwargs(args, hostfwd):
    return dict(mem=args.mem, smp=args.smp, disk_size=args.disk_size, tcg=args.tcg,
                res=parse_res(args.res), disk_bus=args.disk, second_disk=args.second_disk,
                net=not args.no_net, hostfwd=hostfwd, bios=args.bios)


def boot_to_welcome(args, vm, d, res, hostfwd):
    """Fresh boot with input locked; passes when the live session shows a GUI with no keystroke."""
    rc = vmmod.flow_no_keystroke(vm, args.iso, None, None, None, 12, 0.03, [], 3, True,
                                 up_kwargs=vm_kwargs(args, hostfwd))
    vm.input_locked = False
    res.add("boot_no_keystroke", rc == 0, "GUI reached with zero keystrokes" if rc == 0
            else "no GUI screen (see shots/failure.png)")
    if rc != 0:
        raise ScenarioAbort("live session never reached a GUI")
    d.shot("welcome")


def walk_wizard(args, vm, d, res, by_id):
    plan = step_plan({"by_id": by_id, "wifi": args.wifi, "profile": args.profile})
    covered = screens_covered(plan)
    assert covered == list(SCREENS[:-1]), covered
    for item in plan[:-1]:
        nxt = SCREENS[SCREENS.index(item["screen"]) + 1]
        if item["screen"] == "welcome":
            time.sleep(d.t(20))  # the READY prompt appears once the intro finishes; the greeting never rests
        d.advance(item, nxt)
        res.add("step_" + item["screen"], True, "advanced to " + nxt)
        if nxt == args.stop_at:
            d.settle(timeout=10)
            d.shot("stop-at-" + nxt)
            raise ScenarioStop(nxt)
    # review -> install: Enter arms the helper, which asks the compositor to confirm the erase
    d.do(plan[-1]["prep"])
    d.shot("review-typed")
    base = vm.screendump()
    d.do(plan[-1]["go"])
    # A lost Enter release would autorepeat into the erase prompt later and Deny it; tapping
    # another key ends any repeat.
    d.do([("sleep", 1), ("key", "shift")])
    if d.changed(base, 30) is None:
        d.shot("stuck-review")
        raise ScenarioAbort("Enter on review did not start the install (disk name mismatch? expected %s)" % by_id)
    d.settle(timeout=20)
    d.shot("install-start")
    res.add("step_review", True, "install started")


def confirm_erase(vm, d, res):
    """The compositor-drawn prompt: Deny holds focus, so Tab to Erase then Space (Enter never grants)."""
    base = d.last  # the install-start shot: the prompt is a full-screen change from it
    img = d.changed(base, 12, max_frac=0.05)
    if img is None:
        # At 1280x800 the prompt can already be on screen in the install-start shot (it settles
        # after the prompt appears). The prompt is on a trusted-UI timer, so look at the pixels:
        # the dialog panel sits in the middle of the dimmed progress screen.
        cur = vm.screendump()
        dd = it.diff_images(base, cur, 16) if cur is not None else None
        if dd is not None and it.matches(dd, 0.05) and d.is_prompt(base):
            img = base
    if img is None:
        d.shot("no-erase-prompt")
        if d.login_root():
            got = d.sh("journalctl -b --no-pager | tail -60", timeout=60)
            res.notes.append("journal tail at the missing prompt: " + (got[1][-3000:] if got else "?"))
        res.add("erase_prompt", False, "no full-screen prompt appeared after Apply (see shots/no-erase-prompt and notes)")
        raise ScenarioAbort("no trusted erase prompt")
    d.settle(timeout=10, quiet=1.0)
    d.shot("erase-prompt")
    d.do([("key", "tab"), ("sleep", 0.5)])
    d.shot("erase-focus")
    d.do([("key", "spc")])
    res.add("erase_prompt", True, "Erase chosen with Tab, Space")
    time.sleep(2)


def wait_helper_gone(d, timeout, res):
    """Poll the live shell until ec-setup-helper has exited. Returns True on exit."""
    end = time.monotonic() + d.t(timeout)
    seen_run, gone = False, 0
    t0 = time.monotonic()
    while time.monotonic() < end:
        got = d.sh("pgrep -x ec-setup-helper >/dev/null && echo RUN || echo GONE", timeout=40, quiet=True)
        state = got[1].strip().splitlines()[-1] if got and got[1].strip() else "?"
        if state == "RUN":
            seen_run, gone = True, 0
        elif state == "GONE":
            gone += 1
            if gone >= 2 and (seen_run or time.monotonic() - t0 > 120):
                return True
        time.sleep(15)
    return False


def install_and_probe(args, vm, d, res, by_id, pubkey):
    """From the erase prompt to a finished install; leaves the target unmounted. Returns the
    probe sections (hostname, packages, limine, ...) or None."""
    confirm_erase(vm, d, res)
    if not d.login_root():
        res.add("live_shell", False, "tty2 root shell did not come up")
        raise ScenarioAbort("no live shell")
    res.add("live_shell", True, "tty2 root shell, output over ttyS0")
    got = d.sh("journalctl -b --no-pager -t ec-abyss | grep -a 'destructive-action' | cut -c1-200", timeout=60)
    lines = got[1].strip().splitlines() if got else []
    ok = any("answered" in l and "allowed=false" not in l for l in lines)
    res.add("erase_granted", ok, "compositor logged Allow for the erase prompt" if ok else
            "compositor journal: " + (" | ".join(lines)[-400:] or "no destructive-action lines")
            + " (shown with no answer means the requester hung up: confirm.rs half-close vs socket.rs EOF)")
    done = wait_helper_gone(d, 3600, res)
    res.add("install_finished", done, "ec-setup-helper exited" if done else "still running at the timeout")
    if not done:
        raise ScenarioAbort("install did not finish")
    got = d.sh("journalctl -b --no-pager | grep -a -i -E 'ec-setup|helper|pacstrap|pacman' | tail -25", timeout=60)
    if got:
        res.notes.append("install journal tail: " + got[1][-1500:])
    got = d.sh("journalctl -b --no-pager | grep -a -i -E 'unmount|umount|gpg-agent|dirmngr' | tail -12 | cut -c1-220", timeout=60)
    if got:
        res.notes.append("helper unmount journal: " + got[1][-1500:])
    got = d.sh_script("probe.sh", target_probe_script(by_id, pubkey), timeout=180)
    if not got:
        res.add("target_probe", False, "probe script gave no answer")
        return None
    sec = parse_sections(got[1])
    res.add("target_probe", "error" not in sec, sec.get("error", "target mounted, facts read, key planted, unmounted"))
    return sec


def check_target(args, res, sec, floor):
    if not sec or "error" in sec:
        return
    res.notes.append("install log tail: " + sec.get("installlog", "")[-2500:])
    res.notes.append("mounts left by the helper at probe time: " + (sec.get("leftover", "").strip() or "none"))
    lines = [l for l in sec.get("installlog", "").splitlines() if l.strip()]
    last = lines[-1] if lines else ""
    res.add("install_log_ok", bool(lines) and '"failed":true' not in last.replace(" ", ""),
            "last progress line: " + (last[:200] or "install log empty"))
    res.add("target_hostname", sec.get("hostname", "").strip() == HOSTNAME, sec.get("hostname", ""))
    res.add("target_user", sec.get("passwd", "").startswith(USER + ":"), sec.get("passwd", "")[:60])
    boot = sec.get("boot", "")
    res.add("target_boot_files", "vmlinuz-linux" in boot and "BOOTX64.EFI" in boot.upper(),
            boot.replace("\n", " ")[:200])
    ok, detail = eval_limine(sec.get("limine", ""), [k for k in args.kernels.split(",") if k])
    res.add("limine_entries", ok, detail)
    installed = set(sec.get("packages", "").split())
    want = expected_packages(args.profile, floor)
    missing = [p for p in want if p not in installed]
    # Names can be provides/renames (libva-mesa-driver is now mesa's); the booted system's
    # `pacman -T` (packages_installed) is the authoritative check, so this one only records.
    res.add("packages_offline", True, "%d installed, floor+profile all present by name" % len(installed) if not missing
            else "by-name misses, resolved after boot by pacman -T: " + " ".join(missing[:40]))
    res.add("sshd_in_target", "present" in sec.get("sshd", "").split(), "openssh in the finished install")
    if args.wifi:
        res.add("wifi_carried", bool(sec.get("nmconn", "").strip()),
                sec.get("nmconn", "") or "no NetworkManager keyfile with ssid=%s on the target" % SSID)


def reboot_to_disk(args, vm, d, res, ssh):
    # The ISO must stay in the drive while the done screen is up: ec-setup lives on the live
    # squashfs, and ejecting first fills tty1 with SQUASHFS read errors (2026-10-19 calibration).
    d.switch_vt(1)
    d.settle(timeout=20)
    d.shot("done")
    base = vm.screendump()
    try:
        txt = subprocess.run(["tesseract", os.path.join(vm.shots, "%02d-done.png" % (d.n - 1)), "-"],
                             capture_output=True, text=True, timeout=60).stdout
    except (OSError, subprocess.SubprocessError):
        txt = ""
    low = txt.lower()
    res.add("done_screen_success", "installed" in low and "stopped" not in low and "could not" not in low,
            "OCR: " + " ".join(txt.split())[:160])
    restarted = False
    if args.boot_entry != "lts":  # for the menu pick the reset below keeps the timing exact
        # Hold a QMP connection across the click: the guest's own reboot shows up as a RESET
        # event with guest=true. The ISO must stay until then (ejecting while the live system is
        # still shutting down hangs it on SQUASHFS read errors, 2026-10-20 row 1).
        with vm.qmp(timeout=5.0) as q:
            # QMP serves one client at a time, so the click goes through the held connection
            w, h = d.size or vm.screen_size(q)
            vm._abs(q, int((w + 776) / 2 - CLICK_RESTART_BR[0]), int(h - CLICK_RESTART_BR[1]), w, h)
            time.sleep(0.05)
            for down in (True, False):
                q.execute("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": "left"}}])
                time.sleep(0.05)
            deadline = time.monotonic() + d.t(300)
            while time.monotonic() < deadline and not restarted:
                if any(e.get("event") == "RESET" and e.get("data", {}).get("guest") for e in q.events):
                    restarted = True
                    break
                try:
                    r = q._read()
                except QMPError:
                    continue  # read timeout: poll again
                if "event" in r:
                    q.events.append(r)
        if restarted:
            # the firmware boots the ISO again within seconds of the reset, so eject right away
            vm.eject_cd()
        if not restarted:
            d.shot("restart-clicked-no-reaction")
            if d.login_root():
                got = d.sh("journalctl -b --no-pager | grep -a -i -E 'reboot|restart|polkit|logind|ec-setup' | tail -15 | cut -c1-240", timeout=60)
                res.notes.append("journal after the Restart click: " + (got[1][-2500:] if got else "?"))
                d.sh("true", timeout=10, quiet=True)
            d.switch_vt(1)
    else:
        vm.eject_cd()
    if not restarted:
        log("Restart not used or did not react; resetting the VM through QMP")
        with vm.qmp() as q:
            q.execute("system_reset")
    res.add("restart_button", restarted or args.boot_entry == "lts",
            "the Restart button rebooted the VM" if restarted else "Restart not clicked or did not reboot (see 18-done)")
    res.notes.append("reboot by Restart button" if restarted else "reboot by QMP system_reset")
    if args.boot_entry == "lts":
        # Limine's menu waits 2 s: any key stops the countdown. Down until the last entry
        # ("EclipseOS (LTS kernel)"), then Enter; keys before the menu exists are ignored.
        t0 = time.monotonic()
        while time.monotonic() - t0 < 9:
            vm.key("down")
            time.sleep(0.4)
        d.shot("limine-menu")
        vm.key("ret")
    ok = ssh.wait_up(d.t(900))
    res.add("reboot_to_disk", ok, "SSH answered from the installed system" if ok
            else "no SSH within the timeout; see shots")
    if not ok:
        d.shot("no-ssh")
        raise ScenarioAbort("installed system never answered on SSH")


def greeter_login(vm, d, res, ssh, label="login"):
    d.settle(timeout=120, quiet=3.0)
    d.shot("greeter" if label == "login" else "greeter-" + label)
    for attempt in range(3):
        if attempt:
            d.do([("click",) + CLICK_PASSWORD])
        d.do([("type", PASSWORD), ("key", "ret")])
        end = time.monotonic() + d.t(90)
        while time.monotonic() < end:
            rc, out = ssh.run("loginctl list-sessions --no-legend", timeout=20)
            if rc == 0 and session_ready(out, USER):
                d.settle(timeout=60, quiet=3.0)
                d.shot("session" if label == "login" else "session-" + label)
                res.add("session_up_" + label, True, "logged in on seat0")
                return True
            time.sleep(5)
        d.shot("greeter-retry-%d" % (attempt + 1))
    res.add("session_up_" + label, False, "no session for %s after the greeter login" % USER)
    return False


def health(args, res, ssh, floor, label, full):
    sfx = "" if label == "boot0" else "@" + label
    # which packages exist decides which units to demand
    rc, out = ssh.run("pacman -Qq", timeout=60)
    installed = set(out.split()) if rc == 0 else set()
    units = expected_user_units(installed)
    rc, out = ssh.run("sh -s", timeout=60, stdin=health_script(units))
    ok, detail = eval_active(parse_kv(out), units)
    res.add("user_units_active" + sfx, ok, detail)
    rc, out = ssh.run("systemctl is-active %s" % " ".join(SYSTEM_ACTIVE), timeout=30)
    states = dict(zip(SYSTEM_ACTIVE, out.split()))
    ok, detail = eval_active(states, SYSTEM_ACTIVE)
    res.add("system_units_active" + sfx, ok, detail)
    rc, out = ssh.run("systemctl is-enabled %s" % " ".join(SYSTEM_ENABLED), timeout=30)
    ok, detail = eval_active(dict(zip(SYSTEM_ENABLED, out.split())), SYSTEM_ENABLED, want="enabled")
    res.add("system_units_enabled" + sfx, ok, detail)
    rc, out = ssh.run("export XDG_RUNTIME_DIR=/run/user/$(id -u); ec-ctl config validate", timeout=60)
    res.add("ec_ctl_config_validate" + sfx, rc == 0, out.strip()[:300])
    rc, out = ssh.run("journalctl -b -p err --no-pager -q", timeout=60, sudo=True)
    bad = journal_ec_errors(out) if rc == 0 else ["journalctl failed: " + out[:200]]
    res.add("journal_ec_errors" + sfx, not bad, "none" if not bad else "; ".join(bad[:8]))
    if full:
        rc, out = ssh.run("uname -r", timeout=20)
        want_lts = args.boot_entry == "lts"
        res.add("booted_kernel", rc == 0 and ("lts" in out) == want_lts,
                "%s (expected %s)" % (out.strip(), "the LTS kernel" if want_lts else "the default kernel"))
        if args.no_net:
            res.skip("pacman_sync", "--no-net")
        else:
            rc, out = ssh.run("grep -rh -B1 -A3 -i 'eclipseos' /etc/pacman.conf /etc/pacman.d/eclipseos* 2>/dev/null | head -20", timeout=30)
            res.notes.append("[eclipseos] config: " + out.strip()[:500])
            rc, out = ssh.run("grep -rh 'Server' /etc/pacman.conf /etc/pacman.d/ 2>/dev/null | grep -a releases/download/repo-x86_64", timeout=30)
            res.add("eclipseos_repo_url", rc == 0 and "chasebrowndev/EclipseOS/releases/download/repo-x86_64" in out,
                    out.strip()[:200] or "no Server line with the GitHub release URL")
            rc, out = ssh.run("ls /var/lib/pacman/sync/ | tr '\\n' ' '; pacman -T pacman 2>&1 | grep -a ^error", timeout=60, sudo=True)
            res.add("pacman_T_before_sync", "error:" not in out, "first pacman -T on first boot; sync dir + errors: " + out.strip()[:200])
            rc, out = ssh.run("pacman -Sy", timeout=180, sudo=True)
            res.add("pacman_sync", rc == 0, out.strip()[-300:])
        want = expected_packages(args.profile, floor)
        rc, out = ssh.run("pacman -T %s" % " ".join(shlex.quote(p) for p in want), timeout=60)
        res.add("packages_installed", rc == 0 and not out.strip(), "all present" if not out.strip() else "missing: " + out.strip()[:400])
        if args.wifi:
            rc, out = ssh.run("nmcli -t -f NAME,TYPE connection show", timeout=30)
            res.add("wifi_profile_known_to_nm", any(SSID in l and "802-11-wireless" in l for l in out.splitlines()), out.strip()[:200])


def untouched(path, limit=4 << 20):
    """True if a qcow2 holds (almost) nothing: nothing was written to it."""
    r = subprocess.run(["qemu-img", "info", "-U", "--output=json", path], capture_output=True, text=True)
    if r.returncode != 0:
        return False
    return json.loads(r.stdout).get("actual-size", 1 << 60) < limit


def run_installed_flow(args, vm, d, res, hostfwd_port, key, pubkey, floor):
    ssh = Ssh(hostfwd_port, key)
    by_id = args.disk_by_id or expected_by_id(args.disk)
    boot_to_welcome(args, vm, d, res, [(hostfwd_port, 22)])
    if args.wifi:
        prepare_wifi(args, vm, d, res)
    walk_wizard(args, vm, d, res, by_id)
    sec = install_and_probe(args, vm, d, res, by_id, pubkey)
    check_target(args, res, sec, floor)
    if args.second_disk:
        res.add("second_disk_untouched", untouched(vm.disk2), "no writes reached the second disk")
    reboot_to_disk(args, vm, d, res, ssh)
    if not greeter_login(vm, d, res, ssh):
        return
    health(args, res, ssh, floor, "boot0", full=True)
    if args.vt_test:
        vt_regression(vm, d, res, ssh)
    for n in range(1, args.reboot_count + 1):
        ssh.run("systemctl reboot", timeout=20, sudo=True)
        ssh.wait_down(d.t(120))
        label = "reboot%d" % n
        ok = ssh.wait_up(d.t(600))
        res.add("ssh_after_" + label, ok, "")
        if not ok:
            break
        if greeter_login(vm, d, res, ssh, label):
            health(args, res, ssh, floor, label, full=False)


def vt_regression(vm, d, res, ssh):
    """tty3 and back; the session must still take input (Super+E opens the launcher)."""
    rc, out = ssh.run("loginctl show-session $(loginctl list-sessions --no-legend | awk '$3==\"%s\"{print $1;exit}') -p VTNr -p Active" % USER, timeout=20)
    m = re.search(r"VTNr=(\d+)", out)
    vt = int(m.group(1)) if m and int(m.group(1)) > 0 else 1
    res.notes.append("session VT: %s (%s)" % (vt, out.strip().replace("\n", " ")))
    def launcher_up():
        rc, _ = ssh.run("pgrep -x ec-launcher", timeout=20)
        return rc == 0
    d.do([("key", "meta_l+e"), ("sleep", 3)])  # control: input works before the switch
    pre = launcher_up()
    d.shot("vt-before-launcher")
    d.do([("key", "esc"), ("sleep", 2)])
    res.add("vt_input_before", pre, "Super+E opened the launcher before the VT switch" if pre else "launcher did not open before the switch (bind or input broken)")
    d.switch_vt(3)
    time.sleep(3)
    d.shot("vt-tty3")
    d.switch_vt(vt)
    time.sleep(5)
    d.settle(timeout=30)
    d.shot("vt-back")
    d.do([("key", "meta_l+e"), ("sleep", 3)])
    post = launcher_up()
    d.shot("vt-after-launcher")
    res.add("vt_input_after", post, "Super+E opened the launcher after tty3 and back to VT%d" % vt if post
            else "no launcher after the VT round trip: input is dead")
    d.do([("key", "esc")])
    rc, out = ssh.run("journalctl -b --no-pager -t ec-abyss | tail -15", timeout=30, sudo=True)
    res.notes.append("abyss journal after VT: " + out[-1500:])


def prepare_wifi(args, vm, d, res):
    """Bring up the simulated AP from the live shell, then return to the wizard on tty1."""
    if not d.login_root():
        res.add("live_shell", False, "tty2 root shell did not come up")
        raise ScenarioAbort("no live shell")
    got = d.sh_script("ap.sh", wifi_ap_script(), timeout=300)
    ok = bool(got) and got[0] == 0 and SSID in got[1]
    res.add("wifi_ap_up", ok, "hostapd WPA2 on mac80211_hwsim, SSID %s" % SSID if ok
            else "ap script failed: rc=%s %s" % (got[0] if got else "?", (got[1] if got else "")[-300:]))
    if not ok:
        raise ScenarioAbort("could not build the simulated access point")
    if args.polkit_workaround:
        rule = ('polkit.addRule(function(a,s){ if (a.id.indexOf("org.freedesktop.NetworkManager.")==0 && '
                's.user=="liveuser" && s.local && s.active) return polkit.Result.YES; });')
        d.sh_script("pk.sh", "cat >/etc/polkit-1/rules.d/60-test-nm.rules <<'EOF'\n%s\nEOF\necho done" % rule, timeout=30)
        res.notes.append("WORKAROUND: live polkit rule planted for liveuser/NetworkManager (--polkit-workaround)")
    d.switch_vt(1)
    # Coming back from a VT, ec-setup replays the welcome intro and ignores input meanwhile;
    # the READY frame is the first still one.
    d.settle(timeout=150, quiet=4.0)


def refusal_flow(args, vm, d, res, by_id, label):
    """The helper must refuse in preflight: no erase prompt, nothing written to the disk."""
    walk_wizard(args, vm, d, res, by_id)
    d.settle(timeout=90, quiet=3.0)
    d.shot(label + "-outcome")  # the "The install stopped" banner with the reason
    if not d.login_root():
        res.add("live_shell", False, "tty2 root shell did not come up")
        raise ScenarioAbort("no live shell")
    got = d.sh("journalctl -b --no-pager -t ec-abyss | grep -a -c 'destructive-action prompt shown'; "
               "pgrep -x ec-setup-helper >/dev/null && echo RUN || echo GONE; "
               "lsblk -n -o NAME /dev/disk/by-id/%s | wc -l" % by_id, timeout=60)
    lines = got[1].split() if got else []
    res.add("refused_no_erase_prompt", len(lines) >= 1 and lines[0] == "0",
            "the compositor showed no erase prompt" if lines and lines[0] == "0" else "journal said: %s" % lines[:1])
    res.add("helper_exited_cleanly", len(lines) >= 2 and lines[1] == "GONE", "ec-setup-helper stopped on its own")
    res.add("disk_untouched", len(lines) >= 3 and lines[2] == "1", "no partitions on the target disk")
    if args.second_disk:
        res.add("second_disk_untouched", untouched(vm.disk2), "no writes reached the second disk")


def run_nonet_flow(args, vm, d, res, hostfwd_port, key, pubkey, floor):
    """No network: preflight must refuse before any erase prompt and before touching the disk."""
    boot_to_welcome(args, vm, d, res, [])
    refusal_flow(args, vm, d, res, args.disk_by_id or expected_by_id(args.disk), "offline")


def run_smalldisk_flow(args, vm, d, res, hostfwd_port, key, pubkey, floor):
    """A disk under the installer's minimum (16 GiB): offered in the wizard, refused in preflight."""
    boot_to_welcome(args, vm, d, res, [])
    refusal_flow(args, vm, d, res, args.disk_by_id or expected_by_id(args.disk), "smalldisk")

def run_bios_flow(args, vm, d, res, hostfwd_port, key, pubkey, floor):
    """SeaBIOS: the helper must refuse before Confirm (no erase prompt), leaving the disk untouched."""
    by_id = args.disk_by_id or expected_by_id(args.disk)
    boot_to_welcome(args, vm, d, res, [(hostfwd_port, 22)])
    walk_wizard(args, vm, d, res, by_id)
    time.sleep(d.t(20))
    d.shot("bios-outcome")
    if not d.login_root():
        raise ScenarioAbort("no live shell")
    got = d.sh("test -d /sys/firmware/efi && echo EFI || echo BIOS; pgrep -x ec-setup-helper >/dev/null && echo RUN || echo GONE")
    out = got[1].split() if got else []
    res.add("booted_bios", "BIOS" in out, " ".join(out))
    res.add("refused_before_erase_prompt", "GONE" in out,
            "helper exited without waiting on a confirmation" if "GONE" in out else "helper still running: it is waiting for an erase prompt")
    res.add("disk_untouched", untouched(vm.disk), "no writes reached the target disk")


def run(args):
    work = os.path.abspath(args.work)
    vm = VM(work)
    res = Results(args)
    floor = read_floor(args.packages_file) if os.path.isfile(args.packages_file) else []
    if not floor:
        res.notes.append("package floor list not found at %s; only profile packages are checked" % args.packages_file)
    server = None
    try:
        if args.profile not in selectable_profiles() and not args.force_profile:
            res.add("profile_selectable", False,
                    "ec-setup offers only Standard (data::selectable); the %s card is inert" % args.profile)
            return finish(args, vm, res, work)
        os.makedirs(work, exist_ok=True)
        key, pubkey = make_ssh_key(os.path.join(work, "ssh"))
        port = free_port()
        scale = 1 if (not args.tcg and os.access("/dev/kvm", os.R_OK | os.W_OK)) else 4
        d = Driver(vm, scale)
        d.pace = 9.0 if args.bios else (6.0 if parse_res(args.res)[0] >= 2560 else 0.0)
        if not args.no_net:
            server = d.server = ScriptServer({})
        flow = (run_bios_flow if args.bios else run_nonet_flow if args.no_net
                else run_smalldisk_flow if disk_gib(args.disk_size) < MIN_DISK_GIB else run_installed_flow)
        flow(args, vm, d, res, port, key, pubkey, floor)
    except ScenarioStop as e:
        res.add("stopped_at", True, "reached the %s screen, as asked" % e)
    except ScenarioAbort as e:
        res.add("scenario_aborted", False, str(e))
    except (HarnessError, it.ImageError, OSError) as e:
        res.add("harness_error", False, "%s: %s" % (type(e).__name__, e))
    finally:
        if server:
            server.close()
    return finish(args, vm, res, work)


def finish(args, vm, res, work):
    try:
        if not args.keep:
            vm.down()
    except HarnessError as e:
        res.notes.append("vm.down: %s" % e)
    os.makedirs(work, exist_ok=True)
    out = os.path.join(work, "result.json")
    res.write(out)
    bad = [c["name"] for c in res.checks if not c["ok"]]
    print("%s: %d checks, %d failed%s -> %s" % ("PASS" if res.passed else "FAIL", len(res.checks),
                                               len(bad), (" (" + ", ".join(bad) + ")") if bad else "", out))
    return 0 if res.passed else 1


def main(argv=None):
    try:
        args = parse_args(argv)
        return run(args)
    except HarnessError as e:
        print("scenario: error: %s" % e, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
