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


if __name__ == "__main__":
    unittest.main(verbosity=1)
