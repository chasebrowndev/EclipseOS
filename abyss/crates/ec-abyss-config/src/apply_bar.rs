// SPDX-License-Identifier: AGPL-3.0-only
//! `Config::apply_*` for the `bar`, `launcher`, `ui` and `settings` blocks.

use super::*;

impl Config {
    /// `style_set`: this document sets `launcher.style`, which wins over the
    /// deprecated `bar.launcher-style` wherever either sits.
    pub(crate) fn apply_bar(&mut self, node: &KdlNode, style_set: bool) {
        let Some(children) = node.children() else { return };
        let mut seen_pinned_apps = false;
        for n in children.nodes() {
            match n.name().value() {
                "fold-when-inactive" => {
                    self.bar.fold_when_inactive = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "fold-height" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) => self.bar.fold_height = v.clamp(2, 16) as u32,
                    None => self.reject(n, "fold-height expects an integer"),
                },
                "rounding" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) => self.bar.rounding = v.clamp(0, 64) as u32,
                    None => self.reject(n, "rounding expects an integer"),
                },
                "fold-when-idle" => {
                    self.bar.fold_when_idle = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "idle-seconds" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) => self.bar.idle_seconds = v.clamp(5, 600) as u32,
                    None => self.reject(n, "idle-seconds expects an integer"),
                },
                "fold-duration-ms" => match arg(n).and_then(parse_duration_ms) {
                    Some(v) => self.bar.fold_duration_ms = v.min(1000),
                    None => self.reject(n, "fold-duration-ms expects a duration"),
                },
                "fold-curve" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) if EASING_CURVES.contains(&c) => {
                        self.bar.fold_curve = c.to_owned();
                    }
                    other => self.reject(
                        n,
                        format!("unknown bar fold-curve, keeping default (other={:?})", other),
                    ),
                },
                "position" => match arg(n).and_then(KdlValue::as_string) {
                    Some("top") => self.bar.position = BarPosition::Top,
                    Some("bottom") => self.bar.position = BarPosition::Bottom,
                    other => self.reject(
                        n,
                        format!("unknown bar position, keeping default (other={:?})", other),
                    ),
                },
                "tray" => self.apply_bar_tray(n),
                "pinned-apps" => {
                    if seen_pinned_apps {
                        self.reject(
                            n,
                            "repeated pinned-apps is ignored; list every id on a single node",
                        );
                        continue;
                    }
                    seen_pinned_apps = true;
                    self.bar.pinned_apps = self.pinned_app_ids(n);
                }
                "clock" => self.apply_bar_clock(n),
                "widgets" => self.apply_bar_widgets(n),
                "motion" => self.apply_bar_motion(n),
                "widget" => self.apply_bar_widget(n),
                "eye" => {
                    self.bar.eye = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                "popup-anchor" => match arg(n).and_then(KdlValue::as_string) {
                    Some("cell") => self.bar.popup_anchor = BarPopupAnchor::Cell,
                    Some("pointer") => self.bar.popup_anchor = BarPopupAnchor::Pointer,
                    other => self.reject(
                        n,
                        format!(
                            "bar popup-anchor must be \"cell\" or \"pointer\", keeping default (other={:?})",
                            other
                        ),
                    ),
                },
                // Deprecated alias of `launcher.style`: still loads so an
                // old file keeps its launcher, and is not in the schema, so
                // a GUI shows the one key. Writing `launcher.style` removes
                // it (`edit::SUPERSEDES`).
                "launcher-style" if style_set => {
                    tracing::warn!("deprecated: bar.launcher-style is ignored beside launcher.style");
                }
                "launcher-style" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v @ ("centered" | "menu")) => {
                        tracing::warn!("deprecated: bar.launcher-style; write launcher.style instead");
                        self.launcher.style = if v == "menu" {
                            LauncherStyle::Menu
                        } else {
                            LauncherStyle::Centered
                        };
                    }
                    other => self.reject(
                        n,
                        format!(
                            "bar launcher-style must be \"centered\" or \"menu\", keeping default (other={:?})",
                            other
                        ),
                    ),
                },
                _ => self.unknown_key(n, "bar", "bar node"),
            }
        }
    }

    pub(crate) fn apply_bar_clock(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let slot = match n.name().value() {
                "hour-12" => &mut self.bar.clock.hour_12,
                "date-mdy" => &mut self.bar.clock.date_mdy,
                _ => {
                    self.unknown_key(n, "bar.clock", "bar clock node");
                    continue;
                }
            };
            *slot = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
        }
    }

    pub(crate) fn apply_bar_tray(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_pinned = false;
        let mut seen_hidden = false;
        for n in children.nodes() {
            match n.name().value() {
                "pinned" => {
                    // A rejected node is dropped whole, never applied as well
                    // (ADR 0064): the first `pinned` stands.
                    if seen_pinned {
                        self.reject(n, "repeated pinned is ignored; list every id on a single node");
                        continue;
                    }
                    self.bar.tray.pinned = Some(names(n, &mut seen_pinned));
                }
                "hidden" => {
                    if seen_hidden {
                        self.reject(n, "repeated hidden is ignored; list every id on a single node");
                        continue;
                    }
                    self.bar.tray.hidden = names(n, &mut seen_hidden);
                }
                _ => self.unknown_key(n, "bar.tray", "bar tray node"),
            }
        }
        // The built-in applets became widgets (ADR 0065). Their old ids still
        // load so an existing file keeps working, but they mean nothing here
        // any more; say so once per id and point at the fix.
        let tray = &self.bar.tray;
        for id in tray.pinned.iter().flatten().chain(&tray.hidden) {
            if schema::LEGACY_TRAY_BUILTINS.contains(&id.as_str()) {
                tracing::warn!(
                    id = id.as_str(),
                    "deprecated: built-in applet id in bar.tray; list it in bar.widgets.order \
                     instead (ec-ctl config migrate does this)"
                );
            }
        }
    }

    /// `bar { widgets { … } }` (ADR 0065).
    pub(crate) fn apply_bar_widgets(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_order = false;
        let mut seen_important = false;
        for n in children.nodes() {
            match n.name().value() {
                "order" => {
                    if seen_order {
                        self.reject(
                            n,
                            "repeated order replaces the previous one; list every id on a single node",
                        );
                    }
                    seen_order = true;
                    self.bar.widgets.order = self.widget_ids(n);
                }
                "important" => {
                    if seen_important {
                        self.reject(
                            n,
                            "repeated important replaces the previous one; list every id on a single node",
                        );
                    }
                    seen_important = true;
                    self.bar.widgets.important = self.widget_ids(n);
                }
                "now-playing" => self.apply_bar_widget_now_playing(n),
                "system-usage" => self.apply_bar_widget_system_usage(n),
                "volume" => self.apply_bar_widget_volume(n),
                _ => self.unknown_key(n, "bar.widgets", "bar widgets node"),
            }
        }
    }

    /// The ids on a `pinned-apps` node (ADR 0074). A bad id is refused and
    /// dropped, never kept: empty, a path separator, a `.desktop` suffix, or
    /// a repeat of an earlier id (the first stands).
    pub(crate) fn pinned_app_ids(&mut self, n: &KdlNode) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in n.entries() {
            if e.name().is_some() {
                self.reject_entry(e, "pinned app ids are plain strings, not properties");
                continue;
            }
            let Some(id) = e.value().as_string() else {
                self.reject_entry(e, "a pinned app id is a string");
                continue;
            };
            if id.is_empty() {
                self.reject_entry(e, "a pinned app id must not be empty");
            } else if id.contains('/') || id.contains('\\') {
                self.reject_entry(
                    e,
                    format!("pinned app {id:?} is a path; give the desktop-entry id, e.g. \"firefox\""),
                );
            } else if id.ends_with(".desktop") {
                self.reject_entry(
                    e,
                    format!("pinned app {id:?} carries the .desktop suffix; drop it"),
                );
            } else if out.iter().any(|o| o == id) {
                self.reject_entry(e, format!("pinned app {id:?} is listed twice"));
            } else {
                out.push(id.to_owned());
            }
        }
        out
    }

    /// The ids on an `order`/`important` node. Built-in ids are checked now;
    /// `custom:<name>` is queued for [`Self::check_custom_widget_ids`] at the
    /// end of the file. A refused id is dropped, never kept.
    pub(crate) fn widget_ids(&mut self, n: &KdlNode) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in n.entries() {
            if e.name().is_some() {
                self.reject_entry(e, "widget ids are plain strings, not properties");
                continue;
            }
            let Some(id) = e.value().as_string() else {
                self.reject_entry(e, "a widget id is a string");
                continue;
            };
            if out.iter().any(|o| o == id) {
                self.reject_entry(e, format!("widget {id:?} is listed twice"));
                continue;
            }
            if let Some(name) = id.strip_prefix(schema::BAR_WIDGET_CUSTOM_PREFIX) {
                if name.is_empty() {
                    self.reject_entry(e, "custom: needs a widget name, e.g. \"custom:weather\"");
                    continue;
                }
                let span = e.span();
                let (offset, len) = match &self.cur {
                    Some((_, text)) => {
                        let raw = text.get(span.offset()..span.offset() + span.len()).unwrap_or("");
                        let lead = raw.len() - raw.trim_start().len();
                        (span.offset() + lead, raw.trim().len())
                    }
                    None => (span.offset(), span.len()),
                };
                self.pending_custom.push(PendingCustom {
                    name: name.to_owned(),
                    offset,
                    len,
                });
            } else if !schema::BAR_WIDGET_IDS.contains(&id) {
                let message = match nearest(id, schema::BAR_WIDGET_IDS.iter().copied()) {
                    Some(h) => format!("unknown widget {id:?} (did you mean {h:?}?)"),
                    None => format!(
                        "unknown widget {id:?}; built-in widgets are {}, or custom:<name> for a widget block",
                        schema::BAR_WIDGET_IDS.join(", ")
                    ),
                };
                self.reject_entry(e, message);
                continue;
            }
            out.push(id.to_owned());
        }
        out
    }

    /// A bool node: bare is `#true`; anything but a bool is refused.
    pub(crate) fn flag(&mut self, n: &KdlNode) -> Option<bool> {
        match arg(n) {
            None => Some(true),
            Some(v) => match v.as_bool() {
                Some(b) => Some(b),
                None => {
                    self.reject(n, format!("{} expects #true or #false", n.name().value()));
                    None
                }
            },
        }
    }

    /// An integer node within `min..=max`. Out of range is refused, keeping
    /// the default, rather than clamped behind the human's back.
    pub(crate) fn int_in(&mut self, n: &KdlNode, min: u32, max: u32) -> Option<u32> {
        match arg(n).and_then(KdlValue::as_integer) {
            Some(v) if (min as i128..=max as i128).contains(&v) => Some(v as u32),
            Some(v) => {
                self.reject(
                    n,
                    format!(
                        "{} must be {min}..={max}, keeping default (got {v})",
                        n.name().value()
                    ),
                );
                None
            }
            None => {
                self.reject(n, format!("{} expects an integer", n.name().value()));
                None
            }
        }
    }

    /// A duration node (integer ms, or `"2s"`) within `min..=max` ms.
    pub(crate) fn ms_in(&mut self, n: &KdlNode, min: u32, max: u32) -> Option<u32> {
        match arg(n).and_then(parse_duration_ms) {
            Some(v) if (min..=max).contains(&v) => Some(v),
            Some(v) => {
                self.reject(
                    n,
                    format!(
                        "{} must be {min}..={max} ms, keeping default (got {v})",
                        n.name().value()
                    ),
                );
                None
            }
            None => {
                self.reject(n, format!("{} expects a duration", n.name().value()));
                None
            }
        }
    }

    pub(crate) fn apply_bar_widget_now_playing(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "art" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.now_playing.art = b;
                    }
                }
                "visualizer" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.now_playing.visualizer = b;
                    }
                }
                "remote-art" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.now_playing.remote_art = b;
                    }
                }
                _ => self.unknown_key(n, "bar.widgets.now-playing", "now-playing node"),
            }
        }
    }

    pub(crate) fn apply_bar_widget_system_usage(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "interval-ms" => {
                    if let Some(v) = self.ms_in(n, 250, 10_000) {
                        self.bar.widgets.system_usage.interval_ms = v;
                    }
                }
                "gpu" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.system_usage.gpu = b;
                    }
                }
                "disk" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.system_usage.disk = b;
                    }
                }
                "disk-path" => match arg(n).and_then(KdlValue::as_string) {
                    Some(p) if p.starts_with('/') => self.bar.widgets.system_usage.disk_path = p.to_owned(),
                    _ => self.reject(n, "disk-path expects an absolute path, e.g. \"/home\""),
                },
                _ => self.unknown_key(n, "bar.widgets.system-usage", "system-usage node"),
            }
        }
    }

    pub(crate) fn apply_bar_widget_volume(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "step" => {
                    if let Some(v) = self.int_in(n, 1, 25) {
                        self.bar.widgets.volume.step = v;
                    }
                }
                "scroll" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.volume.scroll = b;
                    }
                }
                "max-percent" => {
                    if let Some(v) = self.int_in(n, 100, 150) {
                        self.bar.widgets.volume.max_percent = v;
                    }
                }
                _ => self.unknown_key(n, "bar.widgets.volume", "volume node"),
            }
        }
    }

    /// `bar { motion { … } }` (ADR 0065).
    pub(crate) fn apply_bar_motion(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "enabled" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.motion.enabled = b;
                    }
                }
                "duration-ms" => {
                    if let Some(v) = self.ms_in(n, 0, 2000) {
                        self.bar.motion.duration_ms = v;
                    }
                }
                "curve" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) if schema::BAR_MOTION_CURVES.contains(&c) => self.bar.motion.curve = c.to_owned(),
                    other => self.reject(
                        n,
                        format!(
                            "bar motion curve must be one of {}, keeping default (other={other:?})",
                            schema::BAR_MOTION_CURVES.join(", ")
                        ),
                    ),
                },
                _ => self.unknown_key(n, "bar.motion", "bar motion node"),
            }
        }
    }

    /// The string arguments of an argv node (`exec`, `on-click`, …): at least
    /// one, all strings. `None` after a refusal.
    pub(crate) fn argv(&mut self, n: &KdlNode) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for e in n.entries() {
            match (e.name(), e.value().as_string()) {
                (None, Some(s)) => out.push(s.to_owned()),
                _ => {
                    self.reject_entry(e, format!("{} takes only string arguments", n.name().value()));
                    return None;
                }
            }
        }
        if out.is_empty() || out[0].is_empty() {
            self.reject(
                n,
                format!(
                    "{} needs a command: {} \"<argv0>\" …",
                    n.name().value(),
                    n.name().value()
                ),
            );
            return None;
        }
        Some(out)
    }

    /// `bar { widget "<name>" { … } }` (ADR 0065). The whole block is refused
    /// on any error, so a widget never runs half-configured.
    pub(crate) fn apply_bar_widget(&mut self, node: &KdlNode) {
        let name = match arg(node).and_then(KdlValue::as_string) {
            Some(s) if !s.trim().is_empty() => s.to_owned(),
            _ => {
                self.reject(node, "widget needs a name: widget \"<name>\" { … }");
                return;
            }
        };
        if !self.widget_blocks.contains(&name) {
            self.widget_blocks.push(name.clone());
        }
        let mut ok = true;
        let mut exec = None;
        let mut interval_ms = None;
        let mut stream = false;
        let mut source = None;
        let mut format = None;
        let mut icon = None;
        let mut on_click = None;
        let mut on_scroll_up = None;
        let mut on_scroll_down = None;
        let mut seen: Vec<&str> = Vec::new();
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            let key = n.name().value();
            let Some(form) = schema::find(schema::WIDGET_KEYS, key) else {
                let names = schema::WIDGET_KEYS.iter().map(|f| f.names[0]);
                self.reject(
                    n,
                    match nearest(key, names) {
                        Some(h) => format!("unknown widget node {key:?} (did you mean {h:?}?)"),
                        None => format!("unknown widget node {key:?}"),
                    },
                );
                ok = false;
                continue;
            };
            if seen.contains(&form.names[0]) {
                self.reject(n, format!("{key} given twice in widget {name:?}"));
                ok = false;
                continue;
            }
            seen.push(form.names[0]);
            let good = match form.names[0] {
                "exec" => self.argv(n).map(|a| exec = Some(a)).is_some(),
                "on-click" => self.argv(n).map(|a| on_click = Some(a)).is_some(),
                "on-scroll-up" => self.argv(n).map(|a| on_scroll_up = Some(a)).is_some(),
                "on-scroll-down" => self.argv(n).map(|a| on_scroll_down = Some(a)).is_some(),
                "interval-ms" => self
                    .ms_in(n, schema::WIDGET_MIN_INTERVAL_MS, schema::WIDGET_MAX_INTERVAL_MS)
                    .map(|v| interval_ms = Some(v))
                    .is_some(),
                "stream" => self.flag(n).map(|b| stream = b).is_some(),
                "source" => match arg(n).and_then(KdlValue::as_string) {
                    Some(s) if schema::WIDGET_SOURCES.contains(&s) => {
                        source = Some(s.to_owned());
                        true
                    }
                    other => {
                        let other = other.unwrap_or("");
                        self.reject(
                            n,
                            match nearest(other, schema::WIDGET_SOURCES.iter().copied()) {
                                Some(h) => format!("unknown widget source {other:?} (did you mean {h:?}?)"),
                                None => format!(
                                    "unknown widget source {other:?}; sources are {}",
                                    schema::WIDGET_SOURCES.join(", ")
                                ),
                            },
                        );
                        false
                    }
                },
                "format" | "icon" => match arg(n).and_then(KdlValue::as_string) {
                    Some(s) => {
                        let slot = if form.names[0] == "format" {
                            &mut format
                        } else {
                            &mut icon
                        };
                        *slot = Some(s.to_owned());
                        true
                    }
                    None => {
                        self.reject(n, format!("{key} expects a string"));
                        false
                    }
                },
                // Every WIDGET_KEYS form is matched above; the table and this
                // match are one list, and a form added to one alone must fail.
                other => {
                    self.reject(n, format!("widget node {other:?} is documented but not wired into the parser \u{2014} this is a bug in abyss, not in your config"));
                    false
                }
            };
            ok &= good;
        }
        let kind = match (exec, source) {
            (Some(_), Some(_)) => {
                self.reject(
                    node,
                    format!("widget {name:?} has both exec and source; give it one"),
                );
                return;
            }
            (None, None) => {
                self.reject(
                    node,
                    format!("widget {name:?} needs exec \"<argv0>\" … or source \"<source>\""),
                );
                return;
            }
            (Some(argv), None) => {
                if format.is_some() {
                    self.reject(
                        node,
                        format!("widget {name:?}: format is for source widgets, not exec"),
                    );
                    return;
                }
                match (stream, interval_ms) {
                    (true, Some(_)) => {
                        self.reject(
                            node,
                            format!("widget {name:?}: a stream runs once and never on an interval; drop interval-ms"),
                        );
                        return;
                    }
                    (true, None) => CustomWidgetKind::Stream { argv },
                    (false, i) => CustomWidgetKind::Exec {
                        argv,
                        interval_ms: i.unwrap_or(schema::WIDGET_DEFAULT_INTERVAL_MS),
                    },
                }
            }
            (None, Some(source)) => {
                if stream || interval_ms.is_some() {
                    self.reject(
                        node,
                        format!("widget {name:?}: stream and interval-ms are for exec widgets, not source"),
                    );
                    return;
                }
                CustomWidgetKind::Source {
                    source,
                    format: format.unwrap_or_else(|| "{}".to_owned()),
                }
            }
        };
        if !ok {
            return;
        }
        let w = CustomWidget {
            name,
            kind,
            icon,
            on_click,
            on_scroll_up,
            on_scroll_down,
        };
        match self.bar.custom_widgets.iter_mut().find(|o| o.name == w.name) {
            Some(slot) => *slot = w,
            None => self.bar.custom_widgets.push(w),
        }
    }

    pub(crate) fn apply_launcher(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "style" => match arg(n).and_then(KdlValue::as_string) {
                    Some("centered") => self.launcher.style = LauncherStyle::Centered,
                    Some("menu") => self.launcher.style = LauncherStyle::Menu,
                    other => self.reject(
                        n,
                        format!("launcher style must be \"centered\" or \"menu\", keeping default (other={other:?})"),
                    ),
                },
                "centered" => self.apply_launcher_centered(n),
                "menu" => self.apply_launcher_menu(n),
                "search" => self.apply_launcher_search(n),
                "bind" => self.apply_launcher_bind(n),
                _ => self.unknown_key(n, "launcher", "launcher key"),
            }
        }
    }

    pub(crate) fn apply_launcher_centered(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "width" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (360..=1200).contains(&v) => self.launcher.centered.width = v as u32,
                    _ => self.reject(n, "launcher centered width must be an integer 360..=1200"),
                },
                "max-rows" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (3..=16).contains(&v) => self.launcher.centered.max_rows = v as u32,
                    _ => self.reject(n, "launcher centered max-rows must be an integer 3..=16"),
                },
                "anchor" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(LauncherAnchor::parse)
                {
                    Some(a) => self.launcher.centered.anchor = a,
                    None => self.reject(
                        n,
                        format!(
                            "launcher centered anchor must be one of {}",
                            schema::LAUNCHER_ANCHORS.join(", ")
                        ),
                    ),
                },
                _ => self.unknown_key(n, "launcher.centered", "launcher centered key"),
            }
        }
    }

    pub(crate) fn apply_launcher_menu(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "max-rows" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (3..=16).contains(&v) => self.launcher.menu.max_rows = v as u32,
                    _ => self.reject(n, "launcher menu max-rows must be an integer 3..=16"),
                },
                _ => self.unknown_key(n, "launcher.menu", "launcher menu key"),
            }
        }
    }

    pub(crate) fn apply_launcher_search(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if !matches!(
                name,
                "path-binaries" | "terminal-apps" | "match-descriptions" | "frecency"
            ) {
                self.unknown_key(n, "launcher.search", "launcher search key");
                continue;
            }
            // A bare node is on, as for the other flags.
            let value = match arg(n).map(KdlValue::as_bool) {
                None => true,
                Some(Some(b)) => b,
                Some(None) => {
                    self.reject(n, format!("launcher search {name} expects #true or #false"));
                    continue;
                }
            };
            let search = &mut self.launcher.search;
            match name {
                "path-binaries" => search.path_binaries = value,
                "terminal-apps" => search.terminal_apps = value,
                "frecency" => search.frecency = value,
                _ => search.match_descriptions = value,
            }
        }
    }

    pub(crate) fn apply_oracle_eyes_bind(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if !matches!(name, "select" | "dismiss" | "expand" | "auto-toggle") {
                self.unknown_key(n, "oracle-eyes.bind", "oracle-eyes bind key");
                continue;
            }
            let Some(text) = arg(n).and_then(KdlValue::as_string) else {
                self.reject(
                    n,
                    format!("oracle-eyes bind {name} must be a chord string like \"Super+A\" or \"none\""),
                );
                continue;
            };
            match parse_chord(text) {
                Ok(chord) => {
                    let parsed = LauncherChord {
                        text: text.to_owned(),
                        chord,
                    };
                    let b = &mut self.oracle_eyes.bind;
                    match name {
                        "select" => b.select = parsed,
                        "dismiss" => b.dismiss = parsed,
                        "expand" => b.expand = parsed,
                        _ => b.auto_toggle = parsed,
                    }
                }
                Err(e) => {
                    let message = format!("oracle-eyes bind {name}: {e}");
                    self.reject(n, message);
                }
            }
        }
    }

    pub(crate) fn apply_launcher_bind(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if !matches!(name, "open" | "run") {
                self.unknown_key(n, "launcher.bind", "launcher bind key");
                continue;
            }
            let Some(text) = arg(n).and_then(KdlValue::as_string) else {
                self.reject(
                    n,
                    format!("launcher bind {name} must be a chord string like \"Super+R\" or \"none\""),
                );
                continue;
            };
            match parse_chord(text) {
                Ok(chord) => {
                    let parsed = LauncherChord {
                        text: text.to_owned(),
                        chord,
                    };
                    if name == "open" {
                        self.launcher.bind.open = parsed;
                    } else {
                        self.launcher.bind.run = parsed;
                    }
                }
                Err(e) => {
                    let message = format!("launcher bind {name}: {e}");
                    self.reject(n, message);
                }
            }
        }
    }

    pub(crate) fn apply_ui(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "show-key-hints" => match arg(n).map(KdlValue::as_bool) {
                    None => self.ui.show_key_hints = true,
                    Some(Some(b)) => self.ui.show_key_hints = b,
                    Some(None) => self.reject(n, "ui show-key-hints expects #true or #false"),
                },
                _ => self.unknown_key(n, "ui", "ui key"),
            }
        }
    }

    pub(crate) fn apply_settings(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() != "search" {
                self.unknown_key(n, "settings", "settings key");
                continue;
            }
            let Some(inner) = n.children() else { continue };
            for k in inner.nodes() {
                match k.name().value() {
                    "frecency" => match arg(k).map(KdlValue::as_bool) {
                        None => self.settings.search_frecency = true,
                        Some(Some(b)) => self.settings.search_frecency = b,
                        Some(None) => self.reject(k, "settings search frecency expects #true or #false"),
                    },
                    _ => self.unknown_key(k, "settings.search", "settings search key"),
                }
            }
        }
    }
}
