#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""compare.py A.png B.png [--tolerance N] [--max-frac F] [--max-mean M] [--ignore x,y,w,h]

--tolerance  per-channel delta (0..255) under which a pixel counts as equal (default 8)
--max-frac   allowed fraction of differing pixels for a match (default 0.01)
--max-mean   optional cap on mean absolute channel difference
Exit: 0 match, 1 differ, 2 error. Prints one line: mean_abs, frac_diff, verdict.
"""
import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import imgtools as it  # noqa: E402


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("a")
    p.add_argument("b")
    p.add_argument("--tolerance", type=int, default=8)
    p.add_argument("--max-frac", type=float, default=0.01)
    p.add_argument("--max-mean", type=float, default=None)
    p.add_argument("--ignore", action="append", default=[], metavar="x,y,w,h")
    a = p.parse_args(argv)
    try:
        ia, ib = it.load_image(a.a), it.load_image(a.b)
        res = it.diff_images(ia, ib, a.tolerance, [it.parse_region(r) for r in a.ignore])
    except (OSError, it.ImageError, ValueError) as e:
        print("compare: error: %s" % e, file=sys.stderr)
        return 2
    ok = it.matches(res, a.max_frac, a.max_mean)
    extra = " size_mismatch=%s vs %s" % (res["a_size"], res["b_size"]) if res["size_mismatch"] else ""
    print("mean_abs=%.4f frac_diff=%.6f tolerance=%d max_frac=%g %s%s" % (
        res["mean_abs"], res["frac_diff"], a.tolerance, a.max_frac, "MATCH" if ok else "DIFFER", extra))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
