#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Headless QEMU driver for the EclipseOS live ISO (python3 stdlib only).

The harness starts exactly one QEMU per work dir, records its pid in <work>/qemu.pid and
only ever signals that pid (after confirming /proc/<pid>/cmdline is our QEMU). It never
uses pkill/killall and never touches any other process. It never builds an ISO.
See README.md.
"""
import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import imgtools as it  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))

OVMF_DIRS = ["/usr/share/edk2/x64", "/usr/share/edk2-ovmf/x64", "/usr/share/OVMF/x64",
             "/usr/share/edk2/ovmf", "/usr/share/OVMF", "/usr/share/ovmf", "/usr/share/qemu"]
OVMF_CODE_NAMES = ["OVMF_CODE.4m.fd", "OVMF_CODE.fd"]
OVMF_VARS_NAMES = ["OVMF_VARS.4m.fd", "OVMF_VARS.fd"]


class HarnessError(Exception):
    pass


class QMPError(HarnessError):
    pass


def log(msg):
    print("[vm %s] %s" % (time.strftime("%H:%M:%S"), msg), file=sys.stderr, flush=True)


# ---------------------------------------------------------------- keys

# US layout. Value = (qcode, needs_shift).
_CHAR = {}
for _c in "abcdefghijklmnopqrstuvwxyz":
    _CHAR[_c] = (_c, False)
    _CHAR[_c.upper()] = (_c, True)
for _c in "0123456789":
    _CHAR[_c] = (_c, False)
for _c, _q in zip(")!@#$%^&*(", "0123456789"):
    _CHAR[_c] = (_q, True)
for _c, _q in [("-", "minus"), ("=", "equal"), ("[", "bracket_left"), ("]", "bracket_right"),
               ("\\", "backslash"), (";", "semicolon"), ("'", "apostrophe"), (",", "comma"),
               (".", "dot"), ("/", "slash"), ("`", "grave_accent"), (" ", "spc"),
               ("\n", "ret"), ("\t", "tab")]:
    _CHAR[_c] = (_q, False)
for _c, _q in [("_", "minus"), ("+", "equal"), ("{", "bracket_left"), ("}", "bracket_right"),
               ("|", "backslash"), (":", "semicolon"), ('"', "apostrophe"), ("<", "comma"),
               (">", "dot"), ("?", "slash"), ("~", "grave_accent")]:
    _CHAR[_c] = (_q, True)

KEY_ALIASES = {"enter": "ret", "return": "ret", "space": "spc", "escape": "esc",
               "control": "ctrl", "super": "meta_l", "win": "meta_l", "meta": "meta_l",
               "del": "delete", "pgup": "pgup", "pgdn": "pgdn", "bksp": "backspace",
               "minus": "minus", "plus": "kp_add"}


def char_keys(ch):
    """qcode list (with shift first if needed) for one typed character."""
    if ch not in _CHAR:
        raise HarnessError("cannot type %r on the US layout" % ch)
    q, shift = _CHAR[ch]
    return (["shift", q] if shift else [q])


def parse_combo(s):
    """'ctrl+alt+f2' -> ['ctrl','alt','f2'] (aliases applied)."""
    parts = [p for p in s.lower().split("+") if p]
    if not parts:
        raise HarnessError("empty key combo")
    return [KEY_ALIASES.get(p, p) for p in parts]


# ---------------------------------------------------------------- QMP

class QMP:
    def __init__(self, path, timeout=15.0, connect_timeout=15.0):
        self.path, self.timeout = path, timeout
        self.events = []
        deadline = time.monotonic() + connect_timeout
        last = None
        while True:
            s = socket.socket(socket.AF_UNIX)
            try:
                s.connect(path)
                break
            except OSError as e:
                s.close()
                last = e
                if time.monotonic() > deadline:
                    raise QMPError("cannot connect to QMP socket %s: %s" % (path, last))
                time.sleep(0.2)
        s.settimeout(timeout)
        self.sock = s
        self.f = s.makefile("rw", encoding="utf-8", newline="\n")
        greeting = self._read()
        if "QMP" not in greeting:
            raise QMPError("no QMP greeting")
        self.execute("qmp_capabilities")

    def _read(self):
        try:
            line = self.f.readline()
        except (OSError, socket.timeout) as e:
            raise QMPError("QMP read failed: %s" % e)
        if not line:
            raise QMPError("QMP connection closed")
        return json.loads(line)

    def execute(self, cmd, **args):
        msg = {"execute": cmd}
        if args:
            msg["arguments"] = args
        try:
            self.f.write(json.dumps(msg) + "\n")
            self.f.flush()
        except OSError as e:
            raise QMPError("QMP write failed: %s" % e)
        while True:
            r = self._read()
            if "event" in r:
                self.events.append(r)
                continue
            if "error" in r:
                raise QMPError("%s: %s" % (cmd, r["error"].get("desc", r["error"])))
            if "return" in r:
                return r["return"]

    def close(self):
        try:
            self.f.close()
        except OSError:
            pass
        self.sock.close()

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()


# ---------------------------------------------------------------- VM

def find_ovmf():
    code = os.environ.get("OVMF_CODE")
    vars_ = os.environ.get("OVMF_VARS")
    if code and vars_:
        return code, vars_
    for d in OVMF_DIRS:
        for cn, vn in zip(OVMF_CODE_NAMES, OVMF_VARS_NAMES):  # 4m pairs with 4m
            c, v = os.path.join(d, cn), os.path.join(d, vn)
            if os.path.isfile(c) and os.path.isfile(v):
                return c, v
    raise HarnessError("OVMF not found (install edk2-ovmf or set OVMF_CODE and OVMF_VARS)")


def default_work():
    base = os.environ.get("ECLIPSEOS_VM_WORK")
    if base:
        return base
    return os.path.join(os.environ.get("TMPDIR", "/tmp"), "eclipseos-vm-%d" % os.getuid())


def _inside_repo(path):
    """True if `path` sits under a git checkout (work dirs must never be in the repo)."""
    p = os.path.realpath(path)
    while True:
        if os.path.exists(os.path.join(p, ".git")):
            return True
        parent = os.path.dirname(p)
        if parent == p:
            return False
        p = parent


class VM:
    def __init__(self, work):
        self.work = os.path.abspath(work)
        self.proc = None
        self.input_locked = False  # --no-keystroke: refuse to send any input
        self.disk = os.path.join(self.work, "disk.qcow2")
        self.vars = os.path.join(self.work, "OVMF_VARS.fd")
        self.qmp_path = os.path.join(self.work, "qmp.sock")
        if len(self.qmp_path.encode()) > 100:  # sun_path limit is 108 bytes
            import hashlib
            d = os.path.join(os.environ.get("XDG_RUNTIME_DIR") or "/tmp",
                             "evm-" + hashlib.sha1(self.work.encode()).hexdigest()[:10])
            os.makedirs(d, mode=0o700, exist_ok=True)
            self.qmp_path = os.path.join(d, "qmp.sock")
        self.serial_log = os.path.join(self.work, "serial.log")
        self.qemu_log = os.path.join(self.work, "qemu.log")
        self.pidfile = os.path.join(self.work, "qemu.pid")
        self.shots = os.path.join(self.work, "shots")
        self.accel_file = os.path.join(self.work, "accel")

    # -- process bookkeeping (pid only; never name-based)
    def pid(self):
        try:
            with open(self.pidfile) as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            return None

    @staticmethod
    def _state(pid):
        try:
            with open("/proc/%d/stat" % pid) as f:
                return f.read().rsplit(")", 1)[1].split()[0]
        except (OSError, IndexError):
            return None

    def _alive(self, pid):
        if self.proc is not None and self.proc.pid == pid:
            return self.proc.poll() is None
        st = self._state(pid)
        return st is not None and st != "Z"

    def _is_ours(self, pid):
        """pid alive AND its cmdline names our qmp socket (guards against pid reuse)."""
        if not self._alive(pid):
            return False
        try:
            with open("/proc/%d/cmdline" % pid, "rb") as f:
                return self.qmp_path.encode() in f.read()
        except OSError:
            return False

    def is_running(self):
        p = self.pid()
        return p is not None and self._is_ours(p)

    def accel(self):
        try:
            with open(self.accel_file) as f:
                return f.read().strip()
        except OSError:
            return "unknown"

    # -- lifecycle
    def up(self, iso=None, mem=4096, smp=2, disk_size="20G", tcg=False, fresh=False,
           no_disk=False, res=None):
        if _inside_repo(self.work):
            raise HarnessError("work dir %s is inside a git checkout; pick one outside the repo" % self.work)
        if self.is_running():
            raise HarnessError("VM already running (pid %d) in %s; run `down` first" % (self.pid(), self.work))
        qemu = shutil.which("qemu-system-x86_64")
        if not qemu:
            raise HarnessError("qemu-system-x86_64 not found")
        if iso and not os.path.isfile(iso):
            raise HarnessError("ISO not found: %s" % iso)
        code, vars_src = find_ovmf()
        if fresh and os.path.isdir(self.work):
            for n in ("disk.qcow2", "OVMF_VARS.fd", "serial.log", "qemu.log", "qmp.sock", "qemu.pid"):
                try:
                    os.unlink(os.path.join(self.work, n))
                except FileNotFoundError:
                    pass
        os.makedirs(self.shots, exist_ok=True)
        if not os.path.exists(self.vars):
            shutil.copyfile(vars_src, self.vars)
        if not no_disk and not os.path.exists(self.disk):
            qi = shutil.which("qemu-img")
            if not qi:
                raise HarnessError("qemu-img not found")
            subprocess.run([qi, "create", "-q", "-f", "qcow2", self.disk, disk_size], check=True)
        for stale in (self.qmp_path, self.pidfile):
            try:
                os.unlink(stale)
            except FileNotFoundError:
                pass
        open(self.serial_log, "w").close()

        kvm = (not tcg) and os.access("/dev/kvm", os.R_OK | os.W_OK)
        accel = "kvm" if kvm else "tcg"
        args = [qemu, "-name", "eclipseos-test", "-machine", "q35,accel=" + accel,
                "-cpu", "host" if kvm else "max", "-m", str(mem), "-smp", str(smp),
                "-drive", "if=pflash,format=raw,readonly=on,file=" + code,
                "-drive", "if=pflash,format=raw,file=" + self.vars,
                "-vga", "none",
                "-device", "virtio-vga" + (",xres=%d,yres=%d" % res if res else ""),
                "-device", "virtio-tablet-pci",
                "-device", "virtio-rng-pci",
                "-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0",
                "-display", "none",
                "-serial", "file:" + self.serial_log,
                "-qmp", "unix:%s,server=on,wait=off" % self.qmp_path,
                "-boot", "menu=off"]
        if iso:
            args += ["-drive", "file=%s,media=cdrom,readonly=on,if=none,id=cd" % iso,
                     "-device", "ide-cd,drive=cd,bootindex=1"]
        if not no_disk:
            args += ["-drive", "file=%s,format=qcow2,if=none,id=hd" % self.disk,
                     "-device", "virtio-blk-pci,drive=hd,bootindex=2"]
        with open(self.qemu_log, "wb") as lf:
            self.proc = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=lf, stderr=lf,
                                         start_new_session=True)
        with open(self.pidfile, "w") as f:
            f.write("%d\n" % self.proc.pid)
        with open(self.accel_file, "w") as f:
            f.write(accel + "\n")
        log("started qemu pid %d (%s) work=%s" % (self.proc.pid, accel, self.work))
        deadline = time.monotonic() + 30
        while not os.path.exists(self.qmp_path):
            if self.proc.poll() is not None:
                raise HarnessError("qemu exited early:\n" + self._tail(self.qemu_log))
            if time.monotonic() > deadline:
                self.down()
                raise HarnessError("QMP socket did not appear")
            time.sleep(0.1)
        return self.proc.pid

    @staticmethod
    def _tail(path, n=15):
        try:
            with open(path, errors="replace") as f:
                return "".join(f.readlines()[-n:])
        except OSError:
            return ""

    def _wait_gone(self, pid, secs):
        end = time.monotonic() + secs
        while time.monotonic() < end:
            if not self._alive(pid):
                return True
            time.sleep(0.1)
        return not self._alive(pid)

    def down(self):
        """Stop OUR qemu, by recorded pid only: QMP quit, then SIGTERM, then SIGKILL."""
        pid = self.pid()
        if pid is None or not self._is_ours(pid):
            for n in (self.pidfile, self.qmp_path):
                try:
                    os.unlink(n)
                except FileNotFoundError:
                    pass
            return False
        try:
            with QMP(self.qmp_path, timeout=5, connect_timeout=2) as q:
                q.execute("quit")
        except (HarnessError, OSError):
            pass
        gone = self._wait_gone(pid, 10)
        for sig, wait in ((signal.SIGTERM, 5), (signal.SIGKILL, 5)):
            if gone:
                break
            if self._is_ours(pid):  # re-verify right before every signal
                os.kill(pid, sig)
            gone = self._wait_gone(pid, wait)
        if self.proc is not None and self.proc.pid == pid:
            self.proc.wait(timeout=5)
        if not gone:
            raise HarnessError("qemu pid %d did not exit" % pid)
        for n in (self.pidfile, self.qmp_path):
            try:
                os.unlink(n)
            except FileNotFoundError:
                pass
        log("stopped qemu pid %d" % pid)
        return True

    # -- QMP helpers
    def qmp(self, timeout=15.0):
        return QMP(self.qmp_path, timeout=timeout)

    def _guard_input(self):
        if self.input_locked:
            raise HarnessError("input is locked (--no-keystroke mode): refusing to send input")

    def screendump(self, q=None, out_png=None):
        """Grab the screen as an Image (via a transient PPM); optionally save PNG."""
        own = q is None
        q = q or self.qmp()
        os.makedirs(self.shots, exist_ok=True)
        tmp = os.path.join(self.shots, ".dump-%d.ppm" % os.getpid())
        try:
            q.execute("screendump", filename=tmp)
            with open(tmp, "rb") as f:
                img = it.parse_ppm(f.read())
        finally:
            if own:
                q.close()
            try:
                os.unlink(tmp)
            except FileNotFoundError:
                pass
        if out_png:
            it.save_png(img, out_png)
        return img

    def send_keys(self, keys, hold_ms=40, q=None):
        self._guard_input()
        own = q is None
        q = q or self.qmp()
        try:
            q.execute("send-key", keys=[{"type": "qcode", "data": k} for k in keys],
                      **{"hold-time": hold_ms})
        finally:
            if own:
                q.close()

    def key(self, combo, q=None):
        self.send_keys(parse_combo(combo), q=q)

    def type_text(self, text, delay=0.06):
        self._guard_input()
        with self.qmp() as q:
            for ch in text:
                self.send_keys(char_keys(ch), q=q)
                time.sleep(delay)  # > hold-time so repeated chars register separately

    def screen_size(self, q=None):
        img = self.screendump(q=q)
        return img.w, img.h

    def move(self, x, y, size=None):
        """Absolute pointer move to pixel (x, y); needs the virtio-tablet device."""
        self._guard_input()
        with self.qmp() as q:
            w, h = size or self.screen_size(q)
            self._abs(q, x, y, w, h)

    @staticmethod
    def _abs(q, x, y, w, h):
        ax = max(0, min(32767, round(x * 32767 / max(1, w - 1))))
        ay = max(0, min(32767, round(y * 32767 / max(1, h - 1))))
        q.execute("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": ax}},
            {"type": "abs", "data": {"axis": "y", "value": ay}}])

    def click(self, x=None, y=None, button="left", double=False, size=None):
        self._guard_input()
        with self.qmp() as q:
            if x is not None:
                w, h = size or self.screen_size(q)
                self._abs(q, x, y, w, h)
                time.sleep(0.05)
            for _ in range(2 if double else 1):
                for down in (True, False):
                    q.execute("input-send-event", events=[
                        {"type": "btn", "data": {"down": down, "button": button}}])
                    time.sleep(0.05)

    # -- waiting
    def _try_shot(self, q):
        try:
            return self.screendump(q=q)
        except (QMPError, it.ImageError, OSError):
            return None  # no scanout yet

    def wait_stable(self, timeout, quiet=3.0, interval=1.0, tolerance=8, max_frac=0.002,
                    accept=None, on_shot=None):
        """Wait until the screen (and `accept(img)` if given) holds unchanged for `quiet` s."""
        end = time.monotonic() + timeout
        prev, since = None, None
        with self.qmp() as q:
            while True:
                img = self._try_shot(q)
                now = time.monotonic()
                if img is not None:
                    if on_shot:
                        on_shot(img)
                    same = prev is not None and it.matches(
                        it.diff_images(prev, img, tolerance), max_frac)
                    ok = accept is None or accept(img)
                    if same and ok:
                        since = since if since is not None else now
                        if now - since >= quiet:
                            return img
                    else:
                        since = None
                    prev = img
                if now > end:
                    raise HarnessError("screen did not become stable within %ds" % timeout)
                time.sleep(interval)

    def wait_change(self, timeout, interval=1.0, tolerance=8, max_frac=0.002, baseline=None):
        end = time.monotonic() + timeout
        with self.qmp() as q:
            base = baseline or self._try_shot(q)
            while True:
                time.sleep(interval)
                img = self._try_shot(q)
                if img is not None:
                    if base is None:
                        base = img
                    elif not it.matches(it.diff_images(base, img, tolerance), max_frac):
                        return img
                if time.monotonic() > end:
                    raise HarnessError("screen did not change within %ds" % timeout)

    def wait_image(self, ref, timeout, interval=2.0, tolerance=8, max_frac=0.01, max_mean=None,
                   ignore=(), on_shot=None):
        """Poll until the screen matches `ref` (Image). Returns (img, diff-result)."""
        end = time.monotonic() + timeout
        best = None
        with self.qmp() as q:
            while True:
                img = self._try_shot(q)
                if img is not None:
                    if on_shot:
                        on_shot(img)
                    res = it.diff_images(ref, img, tolerance, ignore)
                    if best is None or res["frac_diff"] < best["frac_diff"]:
                        best = res
                    if it.matches(res, max_frac, max_mean):
                        return img, res
                if time.monotonic() > end:
                    raise HarnessError("no match for reference within %ds (closest frac_diff=%s)" % (
                        timeout, "n/a" if best is None else "%.4f" % best["frac_diff"]))
                time.sleep(interval)


# ---------------------------------------------------------------- high-level flows

def default_timeout(vm, kvm_secs):
    return kvm_secs if vm.accel() == "kvm" else kvm_secs * 4


def flow_no_keystroke(vm, iso, ref_path, record_path, timeout, tolerance, max_frac, ignore,
                      settle, keep):
    """Boot the ISO, send NO input, and assert the live session reaches a GUI screen."""
    vm.up(iso=iso, fresh=True)
    vm.input_locked = True
    timeout = timeout or default_timeout(vm, 300)
    last_png = os.path.join(vm.shots, "last.png")
    ref = it.load_image(ref_path) if ref_path else None
    state = {"n": 0, "console_seen": False, "good": 0, "last": None}
    verdict = None
    try:
        end = time.monotonic() + timeout
        prev = None
        with vm.qmp() as q:
            while True:
                img = vm._try_shot(q)
                if img is not None:
                    state["last"] = img
                    if is_console := it.looks_like_text_console(img):
                        state["console_seen"] = True
                    blank = it.is_blank(img)
                    if ref is not None:
                        res = it.diff_images(ref, img, tolerance, ignore)
                        good = it.matches(res, max_frac)
                        state["detail"] = "frac_diff=%.4f mean_abs=%.2f" % (res["frac_diff"], res["mean_abs"])
                    else:
                        good = not blank and not is_console
                        if good and prev is not None:
                            good = it.matches(it.diff_images(prev, img, tolerance), 0.2)
                        state["detail"] = "nonblack=%.3f" % it.screen_stats(img)["nonblack"]
                    prev = img
                    state["good"] = state["good"] + 1 if good else 0
                    if state["good"] >= settle:
                        verdict = True
                        break
                if time.monotonic() > end:
                    verdict = False
                    break
                time.sleep(2.0)
        img = state["last"]
        if img is not None:
            it.save_png(img, last_png)
        serial = ""
        try:
            with open(vm.serial_log, errors="replace") as f:
                serial = f.read()
        except OSError:
            pass
        if record_path:
            if not verdict or img is None:
                print("RECORD FAILED: never reached a non-console, non-blank screen", file=sys.stderr)
                return 1
            it.save_png(img, record_path)
            print("recorded reference %s (%dx%d)" % (record_path, img.w, img.h))
            return 0
        if verdict:
            print("PASS: live session reached a GUI screen with zero keystrokes (%s%s)" % (
                state.get("detail", ""), ", text console seen earlier" if state["console_seen"] else ""))
        else:
            fail_png = os.path.join(vm.shots, "failure.png")
            if img is not None:
                it.save_png(img, fail_png)
            why = "no screen" if img is None else (
                "still a text console" if it.looks_like_text_console(img) else
                "blank" if it.is_blank(img) else "did not match reference" if ref else "unstable")
            print("FAIL: after %ds with zero keystrokes: %s (%s); last screen %s" % (
                timeout, why, state.get("detail", ""), fail_png))
        if "login:" in serial:
            print("note: serial log contains 'login:' (%s)" % vm.serial_log)
        return 0 if verdict else 1
    finally:
        if not keep:
            vm.down()


def selftest(vm, args):
    """No ISO: boot OVMF with no disk, screendump via QMP, key/pointer, stop by pid."""
    ok = True

    def check(name, cond, detail=""):
        nonlocal ok
        ok = ok and bool(cond)
        print("%s %s %s" % ("ok  " if cond else "FAIL", name, detail))

    check("qemu-system-x86_64 present", shutil.which("qemu-system-x86_64"))
    try:
        code, _ = find_ovmf()
        check("OVMF found", True, code)
    except HarnessError as e:
        check("OVMF found", False, str(e))
        return 1
    pid = vm.up(iso=None, no_disk=True, mem=512, smp=1, tcg=args.tcg, fresh=True)
    try:
        check("qemu running by pid", vm.is_running(), "pid=%d accel=%s" % (pid, vm.accel()))
        timeout = args.timeout or default_timeout(vm, 60)
        with vm.qmp() as q:
            st = q.execute("query-status")
            check("QMP query-status", st.get("status") in ("running", "prelaunch"), st.get("status", ""))
        img = None
        end = time.monotonic() + timeout
        with vm.qmp() as q:
            while time.monotonic() < end:
                img = vm._try_shot(q)
                if img is not None and not it.is_blank(img):
                    break
                time.sleep(1)
        check("firmware drew a non-blank screen", img is not None and not it.is_blank(img),
              "" if img is None else "%dx%d" % (img.w, img.h))
        png = os.path.join(vm.shots, "selftest-1.png")
        shot = vm.screendump(out_png=png)
        size = os.path.getsize(png)
        w, h, colours = it.validate_png_file(png)
        with open(png, "rb") as f:
            sig = f.read(8) == it.PNG_SIG
        check("screendump is a valid non-empty PNG", sig and size > 0 and colours > 1,
              "%dx%d %d bytes ~%d colours" % (w, h, size, colours))
        stable = vm.wait_stable(timeout=timeout, quiet=2.0, interval=1.0)
        check("wait_stable returned", stable is not None)
        vm.key("esc")
        vm.type_text("Ab")
        vm.move(w // 2, h // 2)
        vm.click()
        check("key / type / pointer accepted by QMP", True)
        time.sleep(1.5)
        after = vm.screendump(out_png=os.path.join(vm.shots, "selftest-2.png"))
        res = it.diff_images(stable, after, 8)
        print("info screen after input: frac_diff=%.4f (informational)" % res["frac_diff"])
        check("compare identical", it.matches(it.diff_images(after, after), 0.0))
    except HarnessError as e:
        check("selftest step", False, str(e))
    finally:
        vm.down()
    check("qemu gone and pidfile removed", not os.path.exists(vm.pidfile) and not vm._alive(pid))
    print("SELFTEST %s" % ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


# ---------------------------------------------------------------- CLI

def build_parser():
    p = argparse.ArgumentParser(prog="vm.py", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--work", default=default_work(),
                   help="work dir (disk, OVMF vars, sockets, logs, shots); never inside the repo "
                        "[default: $ECLIPSEOS_VM_WORK or $TMPDIR/eclipseos-vm-UID]")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("up", help="boot QEMU (ISO optional: omit to boot the disk)")
    s.add_argument("--iso")
    s.add_argument("--mem", type=int, default=4096)
    s.add_argument("--smp", type=int, default=2)
    s.add_argument("--disk-size", default="20G")
    s.add_argument("--tcg", action="store_true", help="force TCG even if /dev/kvm is usable")
    s.add_argument("--fresh", action="store_true", help="recreate disk and OVMF vars")
    s.add_argument("--no-disk", action="store_true")
    s.add_argument("--res", help="virtio-gpu resolution WxH")
    sub.add_parser("down", help="stop the VM by its recorded pid")
    sub.add_parser("status")
    s = sub.add_parser("shot"); s.add_argument("out")
    s = sub.add_parser("key", help="e.g. ret, esc, ctrl+alt+f2, shift+tab")
    s.add_argument("combo", nargs="+")
    s = sub.add_parser("type", help="type text (US layout, shift handled); '-' reads stdin")
    s.add_argument("text"); s.add_argument("--delay", type=float, default=0.06)
    s = sub.add_parser("move"); s.add_argument("x", type=int); s.add_argument("y", type=int)
    s = sub.add_parser("click", help="click, optionally after moving to X Y")
    s.add_argument("x", type=int, nargs="?"); s.add_argument("y", type=int, nargs="?")
    s.add_argument("--button", default="left", choices=["left", "right", "middle"])
    s.add_argument("--double", action="store_true")
    s = sub.add_parser("wait-screen", help="wait for the screen to change or become stable")
    s.add_argument("mode", choices=["change", "stable"])
    s.add_argument("--timeout", type=int, default=120)
    s.add_argument("--quiet", type=float, default=3.0, help="stable: seconds unchanged")
    s.add_argument("--tolerance", type=int, default=8)
    s.add_argument("--max-frac", type=float, default=0.002)
    s.add_argument("--save", help="save the resulting screen as PNG")
    s = sub.add_parser("wait-image", help="poll until the screen matches a reference PNG")
    s.add_argument("ref"); s.add_argument("--timeout", type=int, default=120)
    s.add_argument("--tolerance", type=int, default=8)
    s.add_argument("--max-frac", type=float, default=0.01)
    s.add_argument("--max-mean", type=float)
    s.add_argument("--ignore", action="append", default=[], metavar="x,y,w,h")
    s = sub.add_parser("record-ref", help="wait for a stable screen, save it as a reference PNG")
    s.add_argument("out"); s.add_argument("--timeout", type=int, default=120)
    s.add_argument("--quiet", type=float, default=3.0)
    s = sub.add_parser("assert-no-keystroke",
                       help="boot ISO, send NO input, assert a GUI (not `login:`) screen appears")
    s.add_argument("iso")
    s.add_argument("--ref", help="reference PNG the screen must match")
    s.add_argument("--record", metavar="OUT.png",
                   help="instead of asserting, save the reached screen as a reference")
    s.add_argument("--timeout", type=int, help="default 300s (KVM) / 1200s (TCG)")
    s.add_argument("--tolerance", type=int, default=12)
    s.add_argument("--max-frac", type=float, default=0.03)
    s.add_argument("--ignore", action="append", default=[], metavar="x,y,w,h")
    s.add_argument("--settle", type=int, default=3, help="consecutive good polls required")
    s.add_argument("--keep", action="store_true", help="leave the VM running afterwards")
    s = sub.add_parser("selftest", help="boot OVMF with no ISO/disk and exercise the harness")
    s.add_argument("--tcg", action="store_true"); s.add_argument("--timeout", type=int)
    return p


def main(argv=None):
    a = build_parser().parse_args(argv)
    vm = VM(a.work)
    try:
        if a.cmd == "up":
            res = None
            if a.res:
                res = tuple(int(v) for v in a.res.lower().split("x"))
            pid = vm.up(a.iso, a.mem, a.smp, a.disk_size, a.tcg, a.fresh, a.no_disk, res)
            print("pid=%d accel=%s work=%s" % (pid, vm.accel(), vm.work))
        elif a.cmd == "down":
            print("stopped" if vm.down() else "not running")
        elif a.cmd == "status":
            print("running pid=%d accel=%s" % (vm.pid(), vm.accel()) if vm.is_running() else "not running")
            return 0 if vm.is_running() else 1
        elif a.cmd == "shot":
            img = vm.screendump(out_png=a.out)
            print("%s %dx%d" % (a.out, img.w, img.h))
        elif a.cmd == "key":
            for c in a.combo:
                vm.key(c)
                time.sleep(0.1)
        elif a.cmd == "type":
            text = sys.stdin.read().rstrip("\n") if a.text == "-" else a.text.replace("\\n", "\n")
            vm.type_text(text, a.delay)
        elif a.cmd == "move":
            vm.move(a.x, a.y)
        elif a.cmd == "click":
            vm.click(a.x, a.y, a.button, a.double)
        elif a.cmd == "wait-screen":
            if a.mode == "stable":
                img = vm.wait_stable(a.timeout, a.quiet, tolerance=a.tolerance, max_frac=a.max_frac)
            else:
                img = vm.wait_change(a.timeout, tolerance=a.tolerance, max_frac=a.max_frac)
            if a.save:
                it.save_png(img, a.save)
            print(a.mode + " ok")
        elif a.cmd == "wait-image":
            img, res = vm.wait_image(it.load_image(a.ref), a.timeout, tolerance=a.tolerance,
                                     max_frac=a.max_frac, max_mean=a.max_mean,
                                     ignore=[it.parse_region(r) for r in a.ignore])
            print("MATCH frac_diff=%.6f mean_abs=%.4f" % (res["frac_diff"], res["mean_abs"]))
        elif a.cmd == "record-ref":
            img = vm.wait_stable(a.timeout, a.quiet)
            it.save_png(img, a.out)
            print("recorded %s %dx%d" % (a.out, img.w, img.h))
        elif a.cmd == "assert-no-keystroke":
            return flow_no_keystroke(vm, a.iso, a.ref, a.record, a.timeout, a.tolerance,
                                     a.max_frac, [it.parse_region(r) for r in a.ignore],
                                     a.settle, a.keep)
        elif a.cmd == "selftest":
            return selftest(vm, a)
        return 0
    except (HarnessError, it.ImageError, OSError, ValueError) as e:
        print("vm: error: %s" % e, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
