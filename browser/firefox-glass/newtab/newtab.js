// SPDX-License-Identifier: AGPL-3.0-only
//
// Glass new-tab page: clock, search, user-editable pinned sites.

"use strict";

const $ = (id) => document.getElementById(id);
let current = null;
let editing = false;

function tick() {
  const d = new Date();
  $("hh").textContent = String(d.getHours()).padStart(2, "0");
  $("mm").textContent = String(d.getMinutes()).padStart(2, "0");
  $("date").textContent = d
    .toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" })
    .toUpperCase();
}

function normalizeUrl(raw) {
  const t = raw.trim();
  if (!t) return null;
  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(t) ? t : "https://" + t;
  try {
    const u = new URL(withScheme);
    return u.protocol === "http:" || u.protocol === "https:" ? u.href : null;
  } catch (e) {
    return null;
  }
}

function render(settings) {
  const grid = $("grid");
  grid.textContent = "";
  document.querySelector(".pins").classList.toggle("editing", editing);
  $("add").hidden = !editing;
  $("edit").textContent = editing ? "Done" : "Edit";

  if (!settings.pinned.length) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "Nothing pinned. Press Edit to add a site.";
    grid.append(li);
  }
  settings.pinned.forEach((site, i) => {
    const li = document.createElement("li");
    const a = document.createElement("a");
    a.className = "tile";
    a.href = site.url;
    const mark = document.createElement("span");
    mark.className = "mark";
    mark.textContent = (site.title || site.url).replace(/^https?:\/\//, "").charAt(0);
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = site.title || site.url;
    a.append(mark, name);
    const rm = document.createElement("button");
    rm.type = "button";
    rm.className = "remove";
    rm.textContent = "×";
    rm.setAttribute("aria-label", "Remove " + name.textContent);
    rm.addEventListener("click", () => {
      const next = Glass.merge(current);
      next.pinned = next.pinned.filter((_, j) => j !== i);
      Glass.save(next);
    });
    li.append(a, rm);
    grid.append(li);
  });
}

function show(settings) {
  current = settings;
  const pal = Glass.applyPageVars(document.documentElement, settings);
  if (!settings.newtab) {
    // The override is static in the manifest; the closest to "off" is to
    // send the tab to Firefox's own home page.
    Glass.api.tabs.update({ url: "about:home" }).catch(() => {
      for (const el of document.querySelectorAll(".clock, .search, .pins")) el.hidden = true;
      $("off").hidden = false;
    });
    return;
  }
  document.querySelector(".colon").style.color = pal.on("newtab") ? "" : "var(--text-3)";
  render(settings);
}

$("edit").addEventListener("click", () => {
  editing = !editing;
  render(current);
  if (editing) $("add-title").focus();
});

$("add").addEventListener("submit", (e) => {
  e.preventDefault();
  const url = normalizeUrl($("add-url").value);
  if (!url) {
    $("add-url").focus();
    return;
  }
  const title = $("add-title").value.trim() || new URL(url).hostname.replace(/^www\./, "");
  const next = Glass.merge(current);
  next.pinned = next.pinned.concat({ title, url });
  $("add-title").value = "";
  $("add-url").value = "";
  Glass.save(next);
});

$("search").addEventListener("submit", (e) => {
  e.preventDefault();
  const q = $("q").value.trim();
  if (!q) return;
  const asUrl = /\s/.test(q) ? null : normalizeUrl(q);
  const target = asUrl && /\.[a-z]{2,}(\/|$)/i.test(q) ? asUrl : current.searchUrl + encodeURIComponent(q);
  window.location.href = target;
});

tick();
setInterval(tick, 5000);
Glass.watch(show);
