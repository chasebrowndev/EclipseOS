// SPDX-License-Identifier: AGPL-3.0-only
// steam-glass: recolours Steam's hard-coded surfaces by VALUE, not by class.
//
// Steam's desktop CSS is built into hashed class names that change with every
// release, but its palette is a small fixed set (#0e141b, #171d25, #1b2838,
// #23262e, #3d4450, #1a9fff ...). This walks each stylesheet once, and for every
// rule that paints one of those colours appends a same-selector rule with the
// glass equivalent. A Steam update that renames every class still matches.
//
// The injector wraps this file: `CSS` is the add-on's stylesheet, bound by
// ec-steam-glass. Reinjecting replaces the previous install.

const NS = "__ecSteamGlass";
window[NS]?.dispose();

const ACCENT = "#f2c33c";
const ACCENT_TEXT = "#f5cf5c";

// "r,g,b" of an opaque Steam colour -> [r, g, b, a] of the glass it becomes.
// Dark grounds become the compositor's smoked tint; the greys Steam uses for
// raised things become the white-alpha steps from STYLE.md.
const GROUND = new Map([
  ["6,10,14", [0, 0, 0, 0.46]],
  ["14,20,27", [0, 0, 0, 0.42]],
  ["20,26,33", [0, 0, 0, 0.34]],
  ["23,29,37", [0, 0, 0, 0.28]],
  ["27,40,56", [0, 0, 0, 0.28]],
  ["29,31,35", [255, 255, 255, 0.035]],
  ["34,36,42", [255, 255, 255, 0.04]],
  ["36,40,47", [255, 255, 255, 0.05]],
  ["45,51,60", [255, 255, 255, 0.07]],
  ["35,38,46", [255, 255, 255, 0.05]],
  ["45,50,61", [255, 255, 255, 0.07]],
  ["56,58,65", [255, 255, 255, 0.08]],
  ["59,63,72", [255, 255, 255, 0.09]],
  ["60,64,71", [255, 255, 255, 0.09]],
  ["61,68,80", [255, 255, 255, 0.10]],
  ["75,81,92", [255, 255, 255, 0.12]],
  ["72,80,94", [255, 255, 255, 0.12]],
  ["103,112,123", [255, 255, 255, 0.16]],
]);

// MODE is bound by the injector. "glass" leaves the tint translucent for the
// compositor's blur to show through; "backed" is the same tint composited over
// the desktop base #0b0906, STYLE.md's blur-off equivalent, for a window the
// compositor cannot see through.
const BASE = [11, 9, 6];
function paint([r, g, b, a]) {
  if (MODE === "glass") return `rgba(${r},${g},${b},${a})`;
  const mix = (c, k) => Math.round(c * a + k * (1 - a));
  return `rgb(${mix(r, BASE[0])},${mix(g, BASE[1])},${mix(b, BASE[2])})`;
}
const ACCENT_FILL = MODE === "glass" ? "rgba(242,195,60,.14)" : paint([242, 195, 60, 0.14]);

// The glass for an opaque Steam colour "r,g,b": the table, else, for any dark
// blue-leaning grey Steam might add next release, a step chosen by how bright
// it is (darkest -> smoked black tint, lighter -> a stronger white-alpha step).
function ground(key) {
  const known = GROUND.get(key);
  if (known) return known;
  const [r, g, b] = key.split(",").map(Number);
  if (Math.max(r, g, b) > 130 || b < r || b - r > 45) return null;
  const lum = 0.3 * r + 0.59 * g + 0.11 * b;
  if (lum < 30) return [0, 0, 0, 0.4];
  return [255, 255, 255, Math.round(((lum - 30) / 100 * 0.14 + 0.04) * 1000) / 1000];
}

// Steam's surface-ramp variables, by the colour each holds. Re-pointing them
// recolours every rule that reads the variable, which is most of the views.
const RAMP = {
  1: "6,10,14", 2: "14,20,27", 3: "20,26,33", 4: "23,29,37", 5: "29,31,35",
  6: "35,38,46", 7: "45,50,61", 8: "61,68,80", 9: "72,80,94",
};

// Steam's blue is the one accent it has; ours is gold, used for state only.
const BLUE = new Set(["26,159,255", "0,153,209"]);

const COLOR_PROPS = ["background-color", "background"];
const BORDER_PROPS = [
  "border-color", "border-top-color", "border-right-color", "border-bottom-color",
  "border-left-color", "outline-color", "caret-color",
];

// "#0e141b", "#fff", "rgb(14, 20, 27)" -> "14,20,27"; anything else, or a
// colour with alpha, -> null (translucent colours are left as Steam drew them).
function opaque(v) {
  v = v.trim().toLowerCase();
  let m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/.exec(v);
  if (m) {
    const h = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
    return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)).join(",");
  }
  m = /^rgb\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)\s*\)$/.exec(v);
  return m ? `${m[1]},${m[2]},${m[3]}` : null;
}

// A gradient made only of Steam's own greys (its panel and page grounds)
// becomes one flat tint, the tint of its last visible stop: STYLE.md has no
// gradients, and a gradient under glass reads as a muddy wash. A page ground
// fading to a darker grey ends dark; a strip fading to transparent stays a
// light lozenge.
function flatGround(v) {
  if (!v.includes("gradient(")) return null;
  const stops = [...v.matchAll(/rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)(?:[\s,/]+([\d.]+))?/g)]
    .filter((m) => m[4] === undefined || Number(m[4]) > 0)
    .map((m) => `${m[1]},${m[2]},${m[3]}`);
  if (!stops.length || !stops.every((k) => ground(k))) return null;
  return paint(ground(stops[stops.length - 1]));
}

function overrideFor(rule) {
  const s = rule.style;
  const out = [];
  const flat = flatGround(s.getPropertyValue("background-image") || s.getPropertyValue("background"));
  if (flat) out.push(`background:none!important;background-color:${flat}!important`);
  for (const p of COLOR_PROPS) {
    const key = opaque(s.getPropertyValue(p));
    if (!key) continue;
    if (ground(key)) out.push(`background-color:${paint(ground(key))}!important`);
    else if (BLUE.has(key)) out.push(`background-color:${ACCENT_FILL}!important`);
    if (out.length && p === "background") out.push("background-image:none!important");
  }
  const text = opaque(s.getPropertyValue("color"));
  if (text && BLUE.has(text)) out.push(`color:${ACCENT_TEXT}!important`);
  for (const p of BORDER_PROPS) {
    const key = opaque(s.getPropertyValue(p));
    if (key && BLUE.has(key)) out.push(`${p}:${ACCENT}!important`);
  }
  return out.length ? `${rule.selectorText}{${out.join(";")}}` : "";
}

function walk(rules, out) {
  for (const r of rules) {
    if (r.cssRules && !r.selectorText) walk(r.cssRules, out);
    else if (r.style && r.selectorText) {
      const o = overrideFor(r);
      if (o) out.push(o);
    }
  }
}

const done = new WeakSet();
const made = [];

function process(sheet) {
  if (!sheet || done.has(sheet)) return;
  done.add(sheet);
  const out = [];
  try {
    walk(sheet.cssRules, out);
  } catch {
    return; // a cross-origin sheet: not Steam's
  }
  if (!out.length) return;
  const el = document.createElement("style");
  el.dataset.ecSteamGlass = "derived";
  el.textContent = out.join("\n");
  (document.head || document.documentElement).appendChild(el);
  done.add(el.sheet);
  made.push(el);
}

function sweep() {
  for (const s of document.styleSheets) process(s);
}

const base = document.createElement("style");
base.dataset.ecSteamGlass = "base";
base.textContent =
  CSS +
  // The compositor draws the glass, so a client must not paint an opaque
  // ground under it; a window it cannot see through gets the base instead.
  `\nhtml,body,#root{background:${MODE === "glass" ? "transparent" : "#0b0906"}!important}` +
  // Text fields are glass lozenges (STYLE.md: white .05 fill, .10 hairline), so
  // a field keeps its edge on a ground that is now nearly black.
  `\ninput:is([type=text],[type=search],[type=password],:not([type])){background-color:${paint([255, 255, 255, 0.05])}!important;border:1px solid rgba(255,255,255,.10)!important;border-radius:var(--radius-sm)!important;color:#fff!important}` +
  `\ninput:is([type=text],[type=search],[type=password],:not([type])):hover{background-color:${paint([255, 255, 255, 0.08])}!important;border-color:rgba(255,255,255,.14)!important}` +
  `\ninput:is([type=text],[type=search],[type=password],:not([type])):focus{background-color:${paint([255, 255, 255, 0.12])}!important;border-color:${ACCENT}!important;outline:none!important}` +
  "\n:root{" +
  Object.entries(RAMP)
    .map(([n, k]) => `--color-greyneutral-${n}:${paint(GROUND.get(k))}!important`)
    .join(";") +
  "}";
(document.head || document.documentElement).appendChild(base);
made.push(base);
done.add(base.sheet);

// Steam loads its CSS in chunks as views open, so new sheets keep arriving.
const watch = new MutationObserver((muts) => {
  for (const m of muts) {
    for (const n of m.addedNodes) {
      if (n.nodeName === "STYLE") process(n.sheet);
      else if (n.nodeName === "LINK") {
        if (n.sheet) process(n.sheet);
        else n.addEventListener("load", () => process(n.sheet), { once: true });
      }
    }
  }
});
watch.observe(document.documentElement, { childList: true, subtree: true });
sweep();

window[NS] = {
  dispose() {
    watch.disconnect();
    for (const el of made) el.remove();
    delete window[NS];
  },
};
