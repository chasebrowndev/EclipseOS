#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Unit tests for the harness pieces that need no QEMU: QMP client (fake socket),
key mapping, PPM/PNG codec, compare, wait loops, screen heuristics, pid safety."""
import json
import os
import shutil
import socket
import struct
import sys
import tempfile
import threading
import unittest
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compare  # noqa: E402
import imgtools as it  # noqa: E402
import scenario as sc  # noqa: E402
import vm as vmmod  # noqa: E402


def solid(w, h, rgb):
    return it.Image(w, h, bytes(rgb) * (w * h))


def ppm_bytes(img):
    return b"P6\n%d %d\n255\n" % (img.w, img.h) + img.data


class FakeQEMU(threading.Thread):
    """Speaks just enough QMP on a unix socket; records commands; serves screendumps."""

    def __init__(self, path, frames):
        super().__init__(daemon=True)
        self.path, self.frames, self.log, self.n = path, list(frames), [], 0
        self.srv = socket.socket(socket.AF_UNIX)
        self.srv.bind(path)
        self.srv.listen(4)

    def run(self):
        while True:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            f = c.makefile("rw", encoding="utf-8", newline="\n")
            f.write(json.dumps({"QMP": {"version": {}, "capabilities": []}}) + "\n")
            f.flush()
            for line in f:
                m = json.loads(line)
                self.log.append(m)
                cmd = m["execute"]
                f.write(json.dumps({"event": "STOP", "timestamp": {}}) + "\n")  # interleaved event
                if cmd == "screendump":
                    if not self.frames:
                        f.write(json.dumps({"error": {"desc": "no surface"}}) + "\n")
                    else:
                        fr = self.frames[min(self.n, len(self.frames) - 1)]
                        self.n += 1
                        with open(m["arguments"]["filename"], "wb") as o:
                            o.write(ppm_bytes(fr))
                        f.write(json.dumps({"return": {}}) + "\n")
                elif cmd == "bogus":
                    f.write(json.dumps({"error": {"class": "CommandNotFound", "desc": "nope"}}) + "\n")
                else:
                    f.write(json.dumps({"return": {}}) + "\n")
                f.flush()
            c.close()

    def cmds(self, name):
        return [m for m in self.log if m["execute"] == name]


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="evmt-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.vm = vmmod.VM(os.path.join(self.tmp, "w"))
        os.makedirs(self.vm.work)

    def fake(self, frames=()):
        f = FakeQEMU(self.vm.qmp_path, frames)
        f.start()
        self.addCleanup(f.srv.close)
        return f


class TestQMP(Base):
    def test_handshake_events_and_error(self):
        fq = self.fake()
        with self.vm.qmp() as q:
            self.assertEqual(q.execute("query-status"), {})
            self.assertTrue(q.events)  # events skipped, not mistaken for replies
            with self.assertRaises(vmmod.QMPError):
                q.execute("bogus")
        self.assertEqual(fq.log[0]["execute"], "qmp_capabilities")

    def test_connect_failure_is_clear(self):
        with self.assertRaises(vmmod.QMPError):
            vmmod.QMP(os.path.join(self.tmp, "none.sock"), connect_timeout=0.3)


class TestKeys(Base):
    def test_char_mapping(self):
        self.assertEqual(vmmod.char_keys("a"), ["a"])
        self.assertEqual(vmmod.char_keys("A"), ["shift", "a"])
        self.assertEqual(vmmod.char_keys("!"), ["shift", "1"])
        self.assertEqual(vmmod.char_keys("_"), ["shift", "minus"])
        self.assertEqual(vmmod.char_keys("~"), ["shift", "grave_accent"])
        self.assertEqual(vmmod.char_keys("\n"), ["ret"])
        with self.assertRaises(vmmod.HarnessError):
            vmmod.char_keys("é")

    def test_combo(self):
        self.assertEqual(vmmod.parse_combo("Ctrl+Alt+F2"), ["ctrl", "alt", "f2"])
        self.assertEqual(vmmod.parse_combo("enter"), ["ret"])

    def test_type_sends_shift_sequence(self):
        fq = self.fake()
        self.vm.type_text("aB", delay=0)
        sent = [[k["data"] for k in m["arguments"]["keys"]] for m in fq.cmds("send-key")]
        self.assertEqual(sent, [["a"], ["shift", "b"]])

    def test_input_lock(self):
        self.fake()
        self.vm.input_locked = True
        for fn in (lambda: self.vm.key("ret"), lambda: self.vm.type_text("x"),
                   lambda: self.vm.click(), lambda: self.vm.move(1, 1, size=(10, 10))):
            with self.assertRaises(vmmod.HarnessError):
                fn()


class TestPointer(Base):
    def test_move_click_scaling(self):
        fq = self.fake([solid(101, 51, (1, 2, 3))])
        self.vm.click(100, 50, double=True)
        ev = fq.cmds("input-send-event")
        self.assertEqual(ev[0]["arguments"]["events"][0]["data"]["value"], 32767)
        self.assertEqual(ev[0]["arguments"]["events"][1]["data"]["value"], 32767)
        btns = [e["arguments"]["events"][0]["data"]["down"] for e in ev[1:]]
        self.assertEqual(btns, [True, False, True, False])


class TestImages(Base):
    def test_png_roundtrip_and_ppm(self):
        img = it.Image(3, 2, bytes(range(18)))
        self.assertEqual(it.decode_png(it.encode_png(img)).data, img.data)
        self.assertEqual(it.parse_ppm(b"P6\n# c\n3 2\n255\n" + img.data).data, img.data)
        with self.assertRaises(it.ImageError):
            it.parse_ppm(b"P6\n3 2\n255\nshort")

    def test_png_filters(self):
        # 2x2 RGB image encoded with Sub (1), then Up (2) then Paeth-free check
        w = 2
        rows = [bytes([10, 20, 30, 40, 50, 60]), bytes([11, 21, 31, 41, 51, 61])]
        raw0 = b"\x01" + bytes([10, 20, 30, 30, 30, 30])  # Sub
        raw1 = b"\x02" + bytes([1, 1, 1, 1, 1, 1])  # Up
        idat = zlib.compress(raw0 + raw1)

        def chunk(t, p):
            return struct.pack(">I", len(p)) + t + p + struct.pack(">I", zlib.crc32(t + p) & 0xFFFFFFFF)

        png = (it.PNG_SIG + chunk(b"IHDR", struct.pack(">IIBBBBB", w, 2, 8, 2, 0, 0, 0))
               + chunk(b"IDAT", idat) + chunk(b"IEND", b""))
        self.assertEqual(it.decode_png(png).data, b"".join(rows))

    def test_bad_png(self):
        with self.assertRaises(it.ImageError):
            it.decode_png(b"not a png")
        good = bytearray(it.encode_png(solid(2, 2, (1, 1, 1))))
        good[-20] ^= 0xFF
        with self.assertRaises(it.ImageError):
            it.decode_png(bytes(good))

    def test_diff(self):
        a, b = solid(10, 10, (0, 0, 0)), solid(10, 10, (0, 0, 0))
        self.assertEqual(it.diff_images(a, b)["frac_diff"], 0.0)
        d = bytearray(b.data)
        d[0:3] = b"\xff\xff\xff"
        r = it.diff_images(a, it.Image(10, 10, d))
        self.assertAlmostEqual(r["frac_diff"], 0.01)
        self.assertAlmostEqual(r["mean_abs"], 255 / 100)
        self.assertEqual(it.diff_images(a, it.Image(10, 10, d), tolerance=255)["frac_diff"], 0.0)
        self.assertEqual(it.diff_images(a, it.Image(10, 10, d), ignore=[(0, 0, 1, 1)])["frac_diff"], 0.0)
        self.assertTrue(it.diff_images(a, solid(5, 5, (0, 0, 0)))["size_mismatch"])

    def test_compare_cli(self):
        pa, pb = os.path.join(self.tmp, "a.png"), os.path.join(self.tmp, "b.png")
        it.save_png(solid(8, 8, (10, 10, 10)), pa)
        it.save_png(solid(8, 8, (12, 10, 10)), pb)
        self.assertEqual(compare.main([pa, pb, "--tolerance", "5"]), 0)
        self.assertEqual(compare.main([pa, pb, "--tolerance", "0", "--max-frac", "0.5"]), 1)
        self.assertEqual(compare.main([pa, os.path.join(self.tmp, "missing.png")]), 2)

    def test_console_heuristic(self):
        w, h = 200, 100
        d = bytearray(3 * w * h)
        for y in range(2, 40):  # grey text block, top-left
            for x in range(2, 60):
                if (x + y) % 3 == 0:
                    d[3 * (y * w + x):3 * (y * w + x) + 3] = b"\xaa\xaa\xaa"
        self.assertTrue(it.looks_like_text_console(it.Image(w, h, d)))
        self.assertTrue(it.is_blank(solid(w, h, (0, 0, 0))))
        self.assertFalse(it.looks_like_text_console(solid(w, h, (0, 0, 0))))
        gold = solid(w, h, (10, 10, 12))
        g = bytearray(gold.data)
        for y in range(30, 70):
            for x in range(50, 150):
                g[3 * (y * w + x):3 * (y * w + x) + 3] = bytes((212, 175, 55))
        self.assertFalse(it.looks_like_text_console(it.Image(w, h, g)))
        self.assertFalse(it.is_blank(it.Image(w, h, g)))


class TestWaits(Base):
    def test_screendump_and_wait_change(self):
        a, b = solid(20, 20, (0, 0, 0)), solid(20, 20, (200, 200, 0))
        self.fake([a, a, b])
        out = os.path.join(self.tmp, "s.png")
        self.assertEqual(self.vm.screendump(out_png=out).w, 20)
        self.assertEqual(it.validate_png_file(out)[:2], (20, 20))
        img = self.vm.wait_change(5, interval=0.05)
        self.assertEqual(img.data, b.data)

    def test_wait_stable_and_timeouts(self):
        a, b = solid(20, 20, (0, 0, 0)), solid(20, 20, (200, 200, 0))
        self.fake([a, b, b, b, b, b, b, b, b, b, b, b, b])
        img = self.vm.wait_stable(5, quiet=0.2, interval=0.05)
        self.assertEqual(img.data, b.data)
        with self.assertRaises(vmmod.HarnessError):
            self.vm.wait_change(0.3, interval=0.05)  # b forever: never changes

    def test_wait_image(self):
        a, b = solid(20, 20, (0, 0, 0)), solid(20, 20, (0, 90, 200))
        self.fake([a, a, b])
        img, res = self.vm.wait_image(b, 5, interval=0.05)
        self.assertEqual(res["frac_diff"], 0.0)
        with self.assertRaises(vmmod.HarnessError):
            self.vm.wait_image(solid(20, 20, (9, 9, 9)), 0.3, interval=0.05)

    def test_no_scanout_is_tolerated(self):
        self.fake([])  # every screendump errors
        with self.assertRaises(vmmod.HarnessError):
            self.vm.wait_stable(0.3, quiet=0.1, interval=0.05)


class TestPidSafety(Base):
    def test_down_ignores_foreign_pid(self):
        # a pidfile pointing at THIS test process (cmdline lacks our socket) must not be signalled
        with open(self.vm.pidfile, "w") as f:
            f.write("%d\n" % os.getpid())
        self.assertFalse(self.vm.is_running())
        self.assertFalse(self.vm.down())  # returns without signalling anything
        self.assertFalse(os.path.exists(self.vm.pidfile))

    def test_work_inside_repo_refused(self):
        here = os.path.dirname(os.path.abspath(__file__))
        v = vmmod.VM(os.path.join(here, "workdir"))
        with self.assertRaises(vmmod.HarnessError):
            v.up(no_disk=True)

    def test_long_socket_path_shortened(self):
        v = vmmod.VM(os.path.join(self.tmp, "x" * 120))
        self.assertLess(len(v.qmp_path.encode()), 108)


class TestQemuArgs(unittest.TestCase):
    def args(self, **kw):
        base = dict(iso="/i.iso", disk="/d.qcow2")
        base.update(kw)
        return vmmod.build_qemu_args("qemu", "kvm", "/c.fd", "/v.fd", "/q.sock", "/s.log", **base)

    def test_defaults_match_the_old_layout(self):
        a = self.args()
        self.assertIn("virtio-blk-pci,drive=hd,serial=eclipse0,bootindex=2", a)
        self.assertIn("ide-cd,drive=cd,id=cdrom0,bootindex=1", a)
        self.assertEqual(a.count("if=pflash,format=raw,readonly=on,file=/c.fd"), 1)
        self.assertIn("user,id=n0", a)

    def test_nvme_second_disk_hostfwd_bios_no_net(self):
        a = self.args(disk_bus="nvme", disk2="/d2.qcow2", hostfwd=[(2222, 22)])
        self.assertIn("nvme,drive=hd,serial=eclipse0,bootindex=2", a)
        self.assertIn("nvme,drive=hd2,serial=eclipse1", a)
        self.assertIn("user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22", a)
        b = self.args(bios=True, net=False)
        self.assertFalse(any("pflash" in x for x in b))
        self.assertIn("-nic", b)
        self.assertNotIn("-netdev", b)
        with self.assertRaises(vmmod.HarnessError):
            self.args(disk_bus="scsi")

    def test_eject_falls_back_to_tray_commands(self):
        d = tempfile.mkdtemp()
        try:
            p = os.path.join(d, "q.sock")
            fq = FakeQEMU(p, [])
            fq.start()
            v = vmmod.VM(d)
            v.qmp_path = p
            v.eject_cd()
            self.assertEqual([m["execute"] for m in fq.log][-1], "eject")
            self.assertEqual(fq.log[-1]["arguments"], {"id": "cdrom0", "force": True})
        finally:
            shutil.rmtree(d, ignore_errors=True)


class TestScenarioArgs(unittest.TestCase):
    def test_defaults_and_options(self):
        a = sc.parse_args(["--iso", "x.iso"])
        self.assertEqual((a.profile, a.disk, a.res, a.reboot_count), ("standard", "virtio", "1280x800", 0))
        a = sc.parse_args(["--iso", "x", "--profile", "agentic", "--disk", "nvme", "--second-disk", "8G",
                           "--wifi", "--reboot-count", "2", "--res", "1920x1080", "--keep"])
        self.assertEqual((a.profile, a.disk, a.second_disk, a.wifi, a.reboot_count, a.keep),
                         ("agentic", "nvme", "8G", True, 2, True))
        self.assertEqual(sc.parse_res(a.res), (1920, 1080))

    def test_rejections(self):
        for argv in (["--iso", "x", "--res", "big"], ["--iso", "x", "--disk-size", "lots"],
                     ["--iso", "x", "--reboot-count", "-1"], ["--iso", "x", "--wifi", "--no-net"],
                     ["--iso", "x", "--bios", "--wifi"], ["--iso", "x", "--bios", "--no-net"]):
            with self.assertRaises(vmmod.HarnessError, msg=argv):
                sc.parse_args(argv)
        with self.assertRaises(SystemExit):
            sc.parse_args(["--iso", "x", "--profile", "ultra"])

    def test_every_plan_profile_is_known(self):
        here = os.path.dirname(os.path.abspath(__file__))
        src = os.path.join(here, "..", "..", "..", "installer", "crates", "ec-setup-plan", "src", "lib.rs")
        if not os.path.isfile(src):
            self.skipTest("installer sources not in this checkout")
        text = open(src).read()
        body = text.split("pub enum Profile {")[1].split("}")[0]
        names = [w.strip().rstrip(",").lower() for w in body.splitlines()
                 if w.strip() and not w.strip().startswith(("#", "//"))]
        self.assertEqual(sorted(names), sorted(sc.PROFILES))


class TestStepTable(unittest.TestCase):
    ctx = {"by_id": "virtio-eclipse0", "wifi": False}

    def test_covers_every_screen_but_the_last_in_order(self):
        plan = sc.step_plan(self.ctx)
        self.assertEqual(sc.screens_covered(plan), list(sc.SCREENS[:-1]))
        self.assertEqual(plan[0]["go"], [("key", "spc")])

    def test_matches_the_wizard_source(self):
        here = os.path.dirname(os.path.abspath(__file__))
        src = os.path.join(here, "..", "..", "..", "installer", "crates", "ec-setup", "src", "model.rs")
        if not os.path.isfile(src):
            self.skipTest("installer sources not in this checkout")
        text = open(src).read()
        block = text.split("pub const ALL: [Step;")[1].split("];")[0]
        names = [w.strip().rstrip(",").replace("Step::", "").lower() for w in block.splitlines()[1:] if "Step::" in w]
        self.assertEqual(names, list(sc.SCREENS))

    def test_typing_is_never_part_of_a_go(self):
        # `go` must change the screen by itself; typing in `go` would fake a transition
        for item in sc.step_plan(self.ctx):
            self.assertFalse(any(a[0] == "type" for a in item["go"]), item["screen"])

    def test_secrets_and_disk_name_land_where_expected(self):
        plan = {p["screen"]: p for p in sc.step_plan(self.ctx)}
        typed = [a[1] for a in plan["identity"]["prep"] if a[0] == "type"]
        self.assertEqual(typed, [sc.USER, sc.PASSWORD, sc.PASSWORD])
        self.assertEqual([a[1] for a in plan["review"]["prep"] if a[0] == "type"], ["virtio-eclipse0"])
        self.assertIn(("key", "down"), plan["disk"]["prep"])
        click = [a for a in plan["keyboard"]["prep"] if a[0] == "click"][0]
        self.assertTrue(0 < click[1] < 0.15 and 0 < click[2] < 1)

    def test_wifi_adds_the_join_to_the_network_step(self):
        net = {p["screen"]: p for p in sc.step_plan(dict(self.ctx, wifi=True))}["network"]
        self.assertTrue(any(a[0] == "click" for a in net["prep"]))
        self.assertIn(("type", sc.WIFI_PSK), net["prep"])
        plain = {p["screen"]: p for p in sc.step_plan(self.ctx)}["network"]
        self.assertEqual(plain["prep"], [])


class TestDiskNames(unittest.TestCase):
    def test_expected_and_preferred(self):
        self.assertEqual(sc.expected_by_id("virtio"), "virtio-eclipse0")
        self.assertEqual(sc.expected_by_id("nvme", "eclipse1"), "nvme-QEMU_NVMe_Ctrl_eclipse1")
        names = ["nvme-eui.0025", "nvme-QEMU_NVMe_Ctrl_eclipse0_1", "nvme-QEMU_NVMe_Ctrl_eclipse0"]
        self.assertEqual(sc.preferred_by_id(names), "nvme-QEMU_NVMe_Ctrl_eclipse0")
        self.assertEqual(sc.preferred_by_id(["wwn-0x1", "ata-X_1"]), "ata-X_1")
        self.assertIsNone(sc.preferred_by_id([]))

    def test_listing_filter_drops_partitions(self):
        lst = "virtio-eclipse0\nvirtio-eclipse0-part1\nvirtio-eclipse1\n"
        self.assertEqual(sc.disk_ids_from_listing(lst, "eclipse0"), ["virtio-eclipse0"])


class TestChecks(unittest.TestCase):
    def test_units(self):
        inst = {"eclipseos-hyperion", "eclipseos-toasts", "eclipseos-desktop"}
        self.assertEqual(sc.expected_user_units(inst),
                         ["ec-hyperion-bar.service", "ec-toasts.service", "ec-screensaver.service", "ec-pairing.service"])
        self.assertIn("ec-policyd.service", sc.expected_user_units(inst | {"eclipseos-agents"}))
        kv = sc.parse_kv("ec-toasts.service=active\nec-hyperion-bar.service=failed\nnoise\n")
        ok, detail = sc.eval_active(kv, ["ec-toasts.service", "ec-hyperion-bar.service", "ec-x.service"])
        self.assertFalse(ok)
        self.assertIn("ec-hyperion-bar.service=failed", detail)
        self.assertIn("ec-x.service=missing", detail)
        self.assertTrue(sc.eval_active({"a": "active"}, ["a"])[0])
        self.assertTrue(sc.eval_active({"a": "enabled"}, ["a"], want="enabled")[0])

    def test_health_script_lists_every_unit(self):
        s = sc.health_script(["a.service", "b.socket"])
        self.assertIn("systemctl --user is-active a.service", s)
        self.assertIn("XDG_RUNTIME_DIR=/run/user/", s)
        self.assertNotIn("!", s)

    def test_journal_filter(self):
        text = ("Oct 18 host ec-abyss[1]: bad thing\nOct 18 host kernel: ACPI error\n"
                "Oct 18 host systemd[1]: ec-toasts.service: failed\nOct 18 host selection[1]: tec-no\n")
        got = sc.journal_ec_errors(text)
        self.assertEqual(len(got), 2)
        self.assertEqual(sc.journal_ec_errors(text, ignore=[r"ec-abyss"]), [got[1]])

    def test_limine(self):
        conf = ("/EclipseOS\n  path: boot():/vmlinuz-linux\n/EclipseOS (fallback)\n  path: boot():/vmlinuz-linux\n")
        self.assertEqual(sc.limine_kernels(conf), ["linux"])
        ok, detail = sc.eval_limine(conf, ["linux", "linux-lts"])
        self.assertFalse(ok)
        self.assertIn("linux-lts", detail)
        both = conf + "/LTS\n  path: boot():/vmlinuz-linux-lts\n"
        self.assertTrue(sc.eval_limine(both, ["linux", "linux-lts"])[0])
        self.assertFalse(sc.eval_limine("", ["linux"])[0])

    def test_packages(self):
        floor = ["base", "linux"]
        self.assertEqual(sc.expected_packages("standard", floor)[:2], floor)
        self.assertNotIn(sc.AGENTS_PACKAGE, sc.expected_packages("standard", floor))
        self.assertIn(sc.AGENTS_PACKAGE, sc.expected_packages("agentic", floor))
        self.assertIn("eclipseos-hyperion", sc.expected_packages("full", floor))
        self.assertNotIn("eclipseos-hyperion", sc.expected_packages("minimal", floor))
        self.assertEqual(sc.expected_packages("standard", ["base", "eclipseos-toasts"]).count("eclipseos-toasts"), 1)

    def test_floor_file_parses(self):
        if not os.path.isfile(sc.FLOOR_LIST):
            self.skipTest("package list not in this checkout")
        floor = sc.read_floor(sc.FLOOR_LIST)
        self.assertIn("linux", floor)
        self.assertIn("eclipseos-meta", floor)
        self.assertTrue(all(not p.startswith("#") for p in floor))

    def test_session_ready(self):
        out = "  c1 1000 tester seat0 tty1\n  2 0 root - -\n"
        self.assertTrue(sc.session_ready(out, "tester"))
        self.assertFalse(sc.session_ready("  c2 1001 other seat0\n", "tester"))
        self.assertFalse(sc.session_ready("", "tester"))

    def test_selectable_profiles_follow_the_source(self):
        d = tempfile.mkdtemp()
        try:
            p = os.path.join(d, "data.rs")
            open(p, "w").write("pub fn selectable(p: Profile) -> bool {\n    matches!(p, Profile::Standard | Profile::Agentic)\n}\n")
            self.assertEqual(sc.selectable_profiles(p), {"standard", "agentic"})
            self.assertEqual(sc.selectable_profiles(os.path.join(d, "none")), {"standard"})
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_profile_click_only_for_non_default(self):
        std = {p["screen"]: p for p in sc.step_plan({"by_id": "x", "profile": "standard"})}
        self.assertEqual(std["profile"]["prep"], [])
        ag = {p["screen"]: p for p in sc.step_plan({"by_id": "x", "profile": "agentic"})}
        self.assertEqual(ag["profile"]["prep"][0][0], "click")


class TestMarkersAndScripts(unittest.TestCase):
    def test_wrap_and_parse(self):
        line = sc.wrap_command("echo hi; false", "t7")
        self.assertIn("/dev/ttyS0", line)
        self.assertNotIn("!", line)
        log_ = "boot noise\r\n@@B:t7\r\nhi\r\n@@E:t7:1\r\n"
        self.assertEqual(sc.parse_marker(log_, "t7"), (1, "hi\n"))
        self.assertIsNone(sc.parse_marker(log_, "t8"))
        self.assertIsNone(sc.parse_marker("@@B:t7\nhalf", "t7"))
        self.assertEqual(sc.parse_marker("@@B:t1\n@@E:t1:0\n", "t1"), (0, ""))

    def test_sections(self):
        sec = sc.parse_sections("x\n@@S:hostname\neclipse\n@@S:passwd\ntester:x:1000\n\n@@S:end\n")
        self.assertEqual(sec["hostname"], "eclipse")
        self.assertEqual(sec["passwd"], "tester:x:1000")
        self.assertEqual(sec["end"], "")

    def test_probe_script_substitutions(self):
        s = sc.target_probe_script("virtio-eclipse0", "ssh-ed25519 AAAA test")
        self.assertIn("ID=virtio-eclipse0", s)
        self.assertIn("'ssh-ed25519 AAAA test'", s)
        self.assertIn("PasswordAuthentication no", s)
        self.assertIn("systemctl --root=/mnt/t enable sshd", s)
        self.assertLess(s.index("PasswordAuthentication"), s.index("umount -R"))
        self.assertNotIn("%(", s)

    def test_ap_script(self):
        s = sc.wifi_ap_script()
        for needle in ("mac80211_hwsim radios=2", "ssid=eclipse-test", "wpa=2", "CCMP",
                       "wpa_passphrase=" + sc.WIFI_PSK, "hostapd -B", "dnsmasq", "netns"):
            self.assertIn(needle, s)
        self.assertNotIn("%(", s)

    def test_script_server_serves_and_404s(self):
        import urllib.error
        import urllib.request
        srv = sc.ScriptServer({"a.sh": "echo hi\n"})
        try:
            url = "http://127.0.0.1:%d/" % srv.port
            self.assertEqual(urllib.request.urlopen(url + "a.sh", timeout=5).read(), b"echo hi\n")
            srv.files["b.sh"] = "x"
            self.assertEqual(urllib.request.urlopen(url + "b.sh", timeout=5).read(), b"x")
            with self.assertRaises(urllib.error.HTTPError):
                urllib.request.urlopen(url + "nope", timeout=5)
        finally:
            srv.close()


class TestResultsAndSsh(unittest.TestCase):
    def test_results_json_and_verdict(self):
        a = sc.parse_args(["--iso", "x.iso"])
        r = sc.Results(a)
        self.assertFalse(r.passed)  # nothing checked is not a pass
        r.add("a", True)
        r.skip("b", "n/a")
        self.assertTrue(r.passed)
        r.add("c", False, "x" * 5000)
        self.assertFalse(r.passed)
        d = tempfile.mkdtemp()
        try:
            p = os.path.join(d, "result.json")
            r.write(p)
            j = json.load(open(p))
            self.assertEqual([c["name"] for c in j["checks"]], ["a", "b", "c"])
            self.assertFalse(j["pass"])
            self.assertEqual(len(j["checks"][2]["detail"]), 2000)
            self.assertTrue(j["checks"][1]["skipped"])
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_ssh_argv_is_key_only_and_password_stays_off_argv(self):
        s = sc.Ssh(2222, "/k")
        argv = s.argv("true")
        self.assertIn("BatchMode=yes", argv)
        self.assertEqual(argv[-2:], ["tester@127.0.0.1", "true"])
        self.assertIn("2222", argv)
        self.assertNotIn(sc.PASSWORD, " ".join(argv))

    def test_free_port_and_key(self):
        self.assertGreater(sc.free_port(), 0)
        if shutil.which("ssh-keygen"):
            d = tempfile.mkdtemp()
            try:
                key, pub = sc.make_ssh_key(os.path.join(d, "k"))
                self.assertTrue(pub.startswith("ssh-ed25519 "))
                self.assertEqual(sc.make_ssh_key(os.path.join(d, "k"))[1], pub)  # reused, not regenerated
            finally:
                shutil.rmtree(d, ignore_errors=True)

    def test_nonselectable_profile_is_reported_not_run(self):
        d = tempfile.mkdtemp()
        try:
            self.assertEqual(sc.main(["--iso", "x.iso", "--work", os.path.join(d, "w"), "--profile", "full"]), 1)
            j = json.load(open(os.path.join(d, "w", "result.json")))
            self.assertEqual(j["checks"][0]["name"], "profile_selectable")
            self.assertFalse(j["pass"])
        finally:
            shutil.rmtree(d, ignore_errors=True)



class TestProgressBar(unittest.TestCase):
    def img(self, step, w=1920, h=1080):
        data = bytearray(3 * w * h)
        left = (w - 776) // 2
        y = 40
        for i in range(13):
            x0 = left + round(26.1 * i)
            width = 41 if i == step else 21
            v = 255 if i == step else 110
            for x in range(x0, x0 + width):
                j = 3 * (y * w + x)
                data[j:j + 3] = bytes([v, v, v])
        return sc.it.Image(w, h, data)

    def test_step_is_read_from_the_bright_segment(self):
        for step in (0, 4, 12):
            self.assertEqual(sc.bar_index(self.img(step)), step)
        self.assertEqual(sc.bar_index(self.img(5, 1280, 800)), 5)

    def test_no_bar_means_none(self):
        self.assertIsNone(sc.bar_index(sc.it.Image(64, 64, bytes(3 * 64 * 64))))

    def test_targets(self):
        self.assertIsNone(sc.step_bar_target("welcome"))
        self.assertEqual(sc.step_bar_target("language"), 0)
        self.assertEqual(sc.step_bar_target("review"), 12)
        self.assertIsNone(sc.step_bar_target("install"))


class TestDiskGib(unittest.TestCase):
    def test_sizes(self):
        self.assertEqual(sc.disk_gib("8G"), 8)
        self.assertEqual(sc.disk_gib("20g"), 20)
        self.assertEqual(sc.disk_gib("512M"), 0.5)
        self.assertLess(sc.disk_gib("8G"), sc.MIN_DISK_GIB)

if __name__ == "__main__":
    unittest.main(verbosity=1)
