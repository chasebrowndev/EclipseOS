// SPDX-License-Identifier: AGPL-3.0-only
//
// Options page. Edits a local copy of the settings, previews it instantly,
// and writes to storage.sync after a short debounce (sync has a write-rate
// quota; a slider drag must not hit it per pixel). background.js re-applies
// the theme from storage.onChanged, so Firefox follows live.

"use strict";

const $ = (id) => document.getElementById(id);
let state = null;
let saveTimer = null;
// Helper status, written to storage.local by background.js.
let status = null;

function queueSave() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => Glass.save(state), 200);
}

function change(mutate) {
  mutate(state);
  paint();
  queueSave();
}

function setToggle(el, on) {
  el.setAttribute("aria-checked", on ? "true" : "false");
}

function setRange(input, out, value) {
  if (document.activeElement !== input) input.value = value;
  input.style.setProperty("--v", value);
  if (out) out.textContent = value;
}

function buildStatic() {
  for (const b of $("mode").children) {
    b.addEventListener("click", () => change((s) => (s.mode = b.dataset.v)));
  }
  $("tint").addEventListener("input", (e) => change((s) => (s.tint = e.target.value)));
  $("strength").addEventListener("input", (e) =>
    change((s) => (s.strength = Number(e.target.value)))
  );

  for (const a of Glass.ACCENTS) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "sw";
    b.dataset.v = a.value;
    b.title = a.name;
    b.setAttribute("role", "radio");
    b.setAttribute("aria-label", a.name);
    b.style.background = a.value;
    b.addEventListener("click", () => change((s) => (s.accent = a.value)));
    $("accents").append(b);
  }

  for (const r of Glass.REGIONS) {
    const row = document.createElement("div");
    row.className = "row";
    row.dataset.region = r.id;
    row.innerHTML =
      '<div class="what"><b></b><span></span></div>' +
      '<div class="mini">' +
      '<div class="slide"><input type="range" min="0" max="100" aria-label="Opacity"><output></output></div>' +
      '<span class="tag">Accent</span>' +
      '<button type="button" class="toggle" role="switch"></button>' +
      "</div>";
    row.querySelector("b").textContent = r.name;
    row.querySelector(".what span").textContent = r.hint;
    row.querySelector("input").setAttribute("aria-label", r.name + " opacity");
    row.querySelector(".toggle").setAttribute("aria-label", r.name + " accent detail");
    row.querySelector("input").addEventListener("input", (e) =>
      change((s) => (s.regions[r.id].lift = Number(e.target.value)))
    );
    row
      .querySelector(".toggle")
      .addEventListener("click", () =>
        change((s) => (s.regions[r.id].accent = !s.regions[r.id].accent))
      );
    $("regions").append(row);
  }
}

function paintStatus() {
  const st = status || { connected: false };
  $("status").dataset.state = st.connected ? "on" : "off";
  $("status-text").textContent = st.connected ? "Connected to abyss" : "Not connected";
  $("status-detail").textContent = st.connected
    ? "Glass keys and chrome height are reported live; nothing to set up."
    : st.error || "Waiting for the abyss helper.";
}

function paint() {
  const s = state;
  const pal = Glass.applyPageVars(document.documentElement, s);
  for (const b of $("mode").children) {
    b.setAttribute("aria-checked", String(b.dataset.v === s.mode));
  }
  if (document.activeElement !== $("tint")) $("tint").value = s.tint;
  setRange($("strength"), $("strength-v"), s.strength);
  for (const b of $("accents").children) {
    b.setAttribute("aria-checked", String(b.dataset.v === s.accent));
  }
  for (const row of $("regions").children) {
    const r = s.regions[row.dataset.region];
    setRange(row.querySelector("input"), row.querySelector("output"), r.lift);
    setToggle(row.querySelector(".toggle"), r.accent);
  }

  // Preview band uses the exact theme colours.
  const c = Glass.buildTheme(s).colors;
  const pv = $("preview").style;
  pv.setProperty("--pv-strip", c.frame);
  pv.setProperty("--pv-toolbar", c.toolbar);
  pv.setProperty("--pv-tab", c.tab_selected);
  pv.setProperty("--pv-field", c.toolbar_field);
  pv.setProperty("--pv-rim", c.toolbar_field_border);
  pv.setProperty("--pv-text", c.tab_text);
  pv.setProperty("--pv-text2", c.tab_background_text);
  pv.background = c.frame;
  void pal;
}

buildStatic();
Glass.watch((settings) => {
  state = settings;
  paint();
});

Glass.api.storage.local.get("status").then((got) => {
  status = got.status || null;
  paintStatus();
});
Glass.api.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.status) {
    status = changes.status.newValue || null;
    paintStatus();
  }
});
