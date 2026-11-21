# SPDX-License-Identifier: AGPL-3.0-only
"""Static Onest instances from Google Fonts' `Onest[wght].ttf`.

    python3 -I assets/fonts/gen_onest.py path/to/'Onest[wght].ttf' assets/fonts

Needs fontTools. iced picks a weight by family + weight from static faces (a
variable font is one face at its default weight), and cannot switch OpenType
features on, so two things are done here instead of at draw time:

* one static per weight (Light 300 ... ExtraBold 800), family "Onest", the
  subfamily in name IDs 2/4/6;
* the `tnum` alternates (`zero.tf`..`nine.tf`) are made the DEFAULT digits, by
  repointing the cmap. Columns of numbers, clocks and readouts then align with
  no feature support in the renderer. Digits are 672/1000 em wide in every weight.
"""
import sys
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

WEIGHTS = [(300, "Light"), (400, "Regular"), (500, "Medium"), (600, "SemiBold"), (700, "Bold"), (800, "ExtraBold")]
src, out = sys.argv[1], sys.argv[2]
for w, n in WEIGHTS:
    f = instantiateVariableFont(TTFont(src), {"wght": w})
    gsub = f["GSUB"].table
    lookups = {i for fr in gsub.FeatureList.FeatureRecord if fr.FeatureTag == "tnum" for i in fr.Feature.LookupListIndex}
    tnum = {}
    for i in lookups:
        for st in gsub.LookupList.Lookup[i].SubTable:
            st = getattr(st, "ExtSubTable", st)
            tnum.update(getattr(st, "mapping", {}))
    for table in f["cmap"].tables:
        for cp in range(0x30, 0x3A):
            if cp in table.cmap and table.cmap[cp] in tnum:
                table.cmap[cp] = tnum[table.cmap[cp]]
    nm = f["name"]
    for nid in (16, 17, 25):
        nm.removeNames(nameID=nid)
    for pid, eid, lid in ((3, 1, 0x409), (1, 0, 0)):
        nm.setName("Onest", 1, pid, eid, lid)
        nm.setName(n, 2, pid, eid, lid)
        nm.setName("Onest " + n, 4, pid, eid, lid)
        nm.setName("Onest-" + n, 6, pid, eid, lid)
    f["OS/2"].usWeightClass = w
    f.save(f"{out}/Onest-{n}.ttf")
