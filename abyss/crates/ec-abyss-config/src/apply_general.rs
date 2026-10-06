// SPDX-License-Identifier: AGPL-3.0-only
//! `Config::apply_*` for the general, render, clipboard, capture, xwayland,
//! idle, input, misc, components, wallpaper, setup, workspace and output
//! blocks.

use super::*;

impl Config {
    pub(crate) fn apply_general(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "gaps-in" => {
                    if !set_i32(&mut self.general.gaps_in, n) {
                        self.reject(n, "gaps-in expects an integer");
                    }
                }
                "gaps-out" => {
                    if !set_i32(&mut self.general.gaps_out, n) {
                        self.reject(n, "gaps-out expects an integer");
                    }
                }
                "gaps-in-vertical" => {
                    if !set_opt_i32(&mut self.general.gaps_in_vertical, n) {
                        self.reject(n, "gaps-in-vertical expects an integer");
                    }
                }
                "gaps-out-vertical" => {
                    if !set_opt_i32(&mut self.general.gaps_out_vertical, n) {
                        self.reject(n, "gaps-out-vertical expects an integer");
                    }
                }
                "border-size" => {
                    if !set_i32(&mut self.general.border_size, n) {
                        self.reject(n, "border-size expects an integer");
                    }
                }
                "focus-follows-mouse" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.focus_follows_mouse = b;
                    }
                }
                "focus-follows-mouse-across-outputs" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.focus_follows_mouse_across_outputs = b;
                    }
                }
                "cursor-follows-moved-window" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.cursor_follows_moved_window = b;
                    }
                }
                "follow-window-to-workspace" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.follow_window_to_workspace = b;
                    }
                }
                "unfocus-on-empty-workspace" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.unfocus_on_empty_workspace = b;
                    }
                }
                // Removed: hover never takes the keyboard from an on-demand
                // layer now, only a click does (wlr-layer-shell `on_demand`).
                "focus-follows-mouse-layers" => {
                    tracing::warn!("deprecated: general.focus-follows-mouse-layers is ignored");
                }
                "refocus-on-scene-change" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.refocus_on_scene_change = b;
                    }
                }
                "layout" => match arg(n).and_then(KdlValue::as_string) {
                    Some("radiant") => self.general.layout = LayoutKind::Radiant,
                    Some("dwindle") => self.general.layout = LayoutKind::Dwindle,
                    Some("master") => self.general.layout = LayoutKind::Master,
                    other => self.reject(n, format!("unknown layout {other:?}")),
                },
                "floating-placement" => match arg(n).and_then(KdlValue::as_string) {
                    Some("centered") => self.general.floating_placement = FloatingPlacement::Centered,
                    Some("pointer") => self.general.floating_placement = FloatingPlacement::Pointer,
                    Some("cascade") => self.general.floating_placement = FloatingPlacement::Cascade,
                    other => self.reject(n, format!("unknown floating-placement {other:?}")),
                },
                "drop-guides" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.drop_guides = b;
                    }
                }
                "drop-guide-color" => match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                    Some(c) => self.general.drop_guide_color = c,
                    None => self.reject(n, "bad color for \"drop-guide-color\""),
                },
                "drop-edge-band" => {
                    if !set_i32(&mut self.general.drop_edge_band, n) {
                        self.reject(n, "drop-edge-band expects an integer");
                    }
                }
                "col-active-border" | "col-inactive-border" => {
                    match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                        Some(c) if name.starts_with("col-active") => self.general.col_active = c,
                        Some(c) => self.general.col_inactive = c,
                        None => self.reject(n, format!("bad color for {name:?}")),
                    }
                }
                _ => self.unknown_key(n, "general", "general key"),
            }
        }
    }

    pub(crate) fn apply_render(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "direct-scanout" => {
                    self.render.direct_scanout = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                _ => self.unknown_key(n, "render", "render node"),
            }
        }
    }

    pub(crate) fn apply_clipboard(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_allow = false;
        for n in children.nodes() {
            match n.name().value() {
                "data-control-allow" => {
                    if seen_allow {
                        self.reject(n, "repeated data-control-allow replaces the previous one; list every name on a single node");
                    }
                    self.clipboard.data_control_allow = names(n, &mut seen_allow);
                }
                _ => self.unknown_key(n, "clipboard", "clipboard node"),
            }
        }
    }

    pub(crate) fn apply_capture(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_allow = false;
        let mut seen_redact = false;
        let mut seen_hide = false;
        for n in children.nodes() {
            match n.name().value() {
                "allow" => {
                    if seen_allow {
                        self.reject(
                            n,
                            "repeated allow replaces the previous one; list every name on a single node",
                        );
                    }
                    self.capture.allow = names(n, &mut seen_allow);
                }
                "redact-app-id" => {
                    if seen_redact {
                        self.reject(n, "repeated redact-app-id replaces the previous one; list every name on a single node");
                    }
                    self.capture.redact_app_id = names(n, &mut seen_redact);
                }
                "hide-layer" => {
                    if seen_hide {
                        self.reject(
                            n,
                            "repeated hide-layer replaces the previous one; list every pair on a single node",
                        );
                    }
                    seen_hide = true;
                    let mut pairs = Vec::new();
                    for v in args(n) {
                        let pair = v
                            .as_string()
                            .and_then(|a| a.split_once(':'))
                            .filter(|(exe, ns)| !exe.is_empty() && !ns.is_empty());
                        match pair {
                            Some((exe, ns)) => pairs.push((exe.to_string(), ns.to_string())),
                            None => self.reject(n, format!("hide-layer entry {v} must be \"exe:namespace\" with both parts non-empty")),
                        }
                    }
                    self.capture.hide_layer = pairs;
                }
                _ => self.unknown_key(n, "capture", "capture node"),
            }
        }
    }

    /// Every refusal in here is [`FailSafe::XwaylandOff`] (ADR 0064): the
    /// owner was configuring X11 and it did not take, so start without it.
    pub(crate) fn apply_xwayland(&mut self, node: &KdlNode) {
        let before = self.errors.len();
        self.apply_xwayland_children(node);
        for e in &mut self.errors[before..] {
            e.fail_safe = Some(FailSafe::XwaylandOff);
        }
    }

    pub(crate) fn apply_xwayland_children(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "enable" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(b) => self.xwayland.enable = b,
                    None => self.reject(n, "xwayland enable needs a boolean (#true or #false)"),
                },
                "scaling" => match arg(n).and_then(KdlValue::as_string) {
                    Some("client") => self.xwayland.scaling_client = true,
                    Some("compositor") => self.xwayland.scaling_client = false,
                    other => self.reject(
                        n,
                        format!(
                            "unknown xwayland scaling mode, keeping default (other={:?})",
                            other
                        ),
                    ),
                },
                _ => self.unknown_key(n, "xwayland", "xwayland node"),
            }
        }
    }

    pub(crate) fn apply_idle(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            let before = self.errors.len();
            match name {
                "dpms-timeout-seconds" | "lock-timeout-seconds" => {
                    match arg(n).and_then(KdlValue::as_integer) {
                        Some(v) if v >= 0 => {
                            let v = (v as u64 != 0).then_some(v as u64);
                            if name == "dpms-timeout-seconds" {
                                self.idle.dpms_timeout = v;
                            } else {
                                self.idle.lock_timeout = v;
                            }
                        }
                        _ => self.reject(
                            n,
                            format!("idle timeout must be a non-negative integer (node={})", name),
                        ),
                    }
                }
                "lock-command" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) => self.idle.lock_command = Some(c.to_owned()),
                    None => self.reject(n, "idle lock-command needs a string argument"),
                },
                _ => self.unknown_key(n, "idle", "idle node"),
            }
            // ADR 0064, owner decision 2026-09-25: a refused lock setting no
            // longer refuses to start. Auto-lock stays at its default (off)
            // and the summary leads with that. Every refusal in `idle` counts,
            // a misspelt key included (`lock-timout-seconds`), except one about
            // DPMS (`dpms-*`), which is not a protection.
            if self.errors.len() > before && !name.starts_with("dpms") {
                self.fail_safe_last(FailSafe::AutoLockOff);
            }
        }
    }

    /// `misc { scripted-input #false }`. Absent keys keep their defaults; the
    /// scripted-input default is `false` and stays `false` on a malformed value.
    pub(crate) fn apply_input(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "kb-layout" => {
                    if let Some(v) = arg(n).and_then(KdlValue::as_string) {
                        self.input.kb_layout = v.to_string();
                    }
                }
                "kb-variant" => {
                    if let Some(v) = arg(n).and_then(KdlValue::as_string) {
                        self.input.kb_variant = v.to_string();
                    }
                }
                "kb-options" => {
                    self.input.kb_options = arg(n)
                        .and_then(KdlValue::as_string)
                        .filter(|v| !v.is_empty())
                        .map(str::to_string);
                }
                "repeat-rate" => {
                    if !set_i32(&mut self.input.repeat_rate, n) {
                        self.reject(n, "repeat-rate expects an integer");
                    }
                }
                "repeat-delay" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=5_000).contains(&v) => self.input.repeat_delay = v as i32,
                    _ => self.reject(n, "repeat-delay must be an integer 0..=5000"),
                },
                "touchpad" => {
                    for n in n.children().map(|c| c.nodes()).unwrap_or_default() {
                        let t = &mut self.input.touchpad;
                        match touchpad_key(n) {
                            Ok(TouchpadKey::NaturalScroll(b)) => t.natural_scroll = b,
                            Ok(TouchpadKey::TapToClick(b)) => t.tap_to_click = b,
                            Ok(TouchpadKey::TapAndDrag(b)) => t.tap_and_drag = b,
                            Ok(TouchpadKey::Dwt(b)) => t.dwt = b,
                            Ok(TouchpadKey::ClickMethod(m)) => t.click_method = m,
                            Ok(TouchpadKey::ScrollMethod(m)) => t.scroll_method = m,
                            Err(None) => self.unknown_key(n, "input.touchpad", "touchpad key"),
                            Err(Some(msg)) => self.reject(n, msg),
                        }
                    }
                }
                "device" => self.apply_input_device(n),
                "calibration" => self.reject(
                    n,
                    "calibration is per device: write it inside input { device \"<name>\" { … } }",
                ),
                _ => match pointer_key(n) {
                    Ok(PointerKey::AccelProfile(v)) => self.input.accel_profile = v,
                    Ok(PointerKey::AccelSpeed(v)) => self.input.accel_speed = v,
                    Ok(PointerKey::ScrollMethod(v)) => self.input.scroll_method = v,
                    // Only `calibration` yields a matrix, and it is refused above.
                    Ok(PointerKey::Calibration(_)) => {}
                    Err(None) => self.unknown_key(n, "input", "input key"),
                    Err(Some(msg)) => self.reject(n, msg),
                },
            }
        }
    }

    /// `input { device "<name>" { .. } }`: any subset of the pointer keys and
    /// of `touchpad { }`, plus `calibration`. A later block for the same name
    /// overrides key by key.
    pub(crate) fn apply_input_device(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string).filter(|s| !s.is_empty()) else {
            self.reject(
                node,
                "input.device needs a libinput device name, e.g. device \"ELAN0001:00 04F3:3140 Touchpad\" { … }",
            );
            return;
        };
        let mut d = self
            .input
            .devices
            .iter()
            .find(|d| d.name == name)
            .cloned()
            .unwrap_or_else(|| InputDevice {
                name: name.to_owned(),
                ..InputDevice::default()
            });
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            if n.name().value() == "touchpad" {
                for n in n.children().map(|c| c.nodes()).unwrap_or_default() {
                    let t = &mut d.touchpad;
                    match touchpad_key(n) {
                        Ok(TouchpadKey::NaturalScroll(b)) => t.natural_scroll = Some(b),
                        Ok(TouchpadKey::TapToClick(b)) => t.tap_to_click = Some(b),
                        Ok(TouchpadKey::TapAndDrag(b)) => t.tap_and_drag = Some(b),
                        Ok(TouchpadKey::Dwt(b)) => t.dwt = Some(b),
                        Ok(TouchpadKey::ClickMethod(m)) => t.click_method = Some(m),
                        Ok(TouchpadKey::ScrollMethod(m)) => t.scroll_method = Some(m),
                        Err(None) => self.reject(
                            n,
                            format!(
                                "unknown input.device.touchpad key {:?}; expected natural-scroll, \
                                 tap-to-click, tap-and-drag, dwt, click-method or scroll-method",
                                n.name().value()
                            ),
                        ),
                        Err(Some(msg)) => self.reject(n, msg),
                    }
                }
                continue;
            }
            match pointer_key(n) {
                Ok(PointerKey::AccelProfile(v)) => d.accel_profile = Some(v),
                Ok(PointerKey::AccelSpeed(v)) => d.accel_speed = Some(v),
                Ok(PointerKey::ScrollMethod(v)) => d.scroll_method = Some(v),
                Ok(PointerKey::Calibration(m)) => d.calibration = Some(m),
                Err(None) => self.reject(
                    n,
                    format!(
                        "unknown input.device key {:?}; expected accel-profile, accel-speed, \
                         scroll-method, calibration or touchpad",
                        n.name().value()
                    ),
                ),
                Err(Some(msg)) => self.reject(n, msg),
            }
        }
        match self.input.devices.iter_mut().find(|x| x.name == d.name) {
            Some(slot) => *slot = d,
            None => self.input.devices.push(d),
        }
    }

    pub(crate) fn apply_misc(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "scripted-input" => {
                    if !self.owned_here(n, "\"misc.scripted-input\"", schema::Owner::Policy) {
                        continue;
                    }
                    self.misc.scripted_input = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "render-device" => match arg(n).and_then(KdlValue::as_string) {
                    Some("auto") => self.misc.render_device = None,
                    Some(v) => self.misc.render_device = Some(v.to_owned()),
                    // ADR 0064 / ADR 0033: dropping it would fall back to auto.
                    None => self.reject_fatal(n, "render-device needs a string"),
                },
                "terminal-command" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) => self.misc.terminal_command = Some(c.to_owned()),
                    None => self.reject(n, "terminal-command needs a string argument"),
                },
                // COMP-13 §1.1 (amended C-05): X11 is its own top-level node.
                // A refusal about X11 all the same: fail safe, as in
                // `apply_xwayland` (ADR 0064).
                "xwayland" => {
                    self.reject(
                        n,
                        "misc.xwayland is not a key; use the top-level `xwayland { enable #false }` node",
                    );
                    self.fail_safe_last(FailSafe::XwaylandOff);
                }
                _ => self.unknown_key(n, "misc", "misc key"),
            }
        }
    }

    pub(crate) fn apply_components(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            let (catalog, slot) = match name {
                "bar" => (schema::COMPONENT_BARS, &mut self.components.bar),
                "launcher" => (schema::COMPONENT_LAUNCHERS, &mut self.components.launcher),
                "notifications" => (
                    schema::COMPONENT_NOTIFICATIONS,
                    &mut self.components.notifications,
                ),
                "control-center" => (
                    schema::COMPONENT_CONTROL_CENTERS,
                    &mut self.components.control_center,
                ),
                _ => {
                    self.unknown_key(n, "components", "components key");
                    continue;
                }
            };
            let value = arg(n).and_then(KdlValue::as_string).map(|v| {
                schema::LEGACY_COMPONENT_IDS
                    .iter()
                    .find_map(|&(old, new)| (old == v).then_some(new))
                    .unwrap_or(v)
            });
            match value {
                Some(v) if catalog.contains(&v) => *slot = v.to_owned(),
                other => {
                    let msg = format!(
                        "components.{name}: unknown value (other={other:?}); expected one of {}",
                        catalog.join(", ")
                    );
                    self.reject(n, msg);
                }
            }
        }
    }

    /// `annotations { accent "#f2c33c"; panel-tint "#17140fcc"; … }`. A bad
    /// colour is refused and that key keeps its value. `panel-tint` alpha is
    /// raised to [`PANEL_TINT_MIN_ALPHA`], not refused: the text on the panel
    /// is white, and on a white page a thinner tint leaves it unreadable.
    pub(crate) fn apply_annotations(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if self.annotations.slot_mut(name).is_none() {
                self.unknown_key(n, "annotations", "annotations key");
                continue;
            }
            match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                Some(c) => {
                    if let Some(slot) = self.annotations.slot_mut(name) {
                        *slot = c;
                    }
                }
                None => self.reject(
                    n,
                    format!("bad color for \"annotations {name}\"; expected \"#rrggbb\" or \"#rrggbbaa\""),
                ),
            }
        }
        let tint = &mut self.annotations.panel_tint[3];
        *tint = tint.max(PANEL_TINT_MIN_ALPHA);
    }

    /// `oracle-eyes { model-command "claude"; timeout-ms 30000; … }`. A bad
    /// value is refused and that key keeps its value. Whether the model
    /// command may run is not decided here: `ec-abyss` withholds one that
    /// is neither the default nor approved (ADR 0067's path).
    pub(crate) fn apply_oracle_eyes(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "model-command" => {
                    let argv: Option<Vec<String>> = n
                        .entries()
                        .iter()
                        .map(|e| {
                            e.name()
                                .is_none()
                                .then(|| e.value().as_string().map(str::to_owned))
                                .flatten()
                        })
                        .collect();
                    match argv {
                        Some(argv) if argv.first().is_some_and(|a| !a.trim().is_empty()) => {
                            self.oracle_eyes.model_command = Some(argv)
                        }
                        _ => self.reject(
                            n,
                            "oracle-eyes model-command takes the program and its arguments as strings, \
                             e.g. model-command \"claude\"",
                        ),
                    }
                }
                "timeout-ms" => {
                    let (lo, hi) = ORACLE_EYES_TIMEOUT_MS;
                    if let Some(v) = self.int_in(n, lo, hi) {
                        self.oracle_eyes.timeout_ms = v;
                    }
                }
                "auto-interval-ms" => {
                    let (lo, hi) = ORACLE_EYES_AUTO_INTERVAL_MS;
                    if let Some(v) = self.int_in(n, lo, hi) {
                        self.oracle_eyes.auto_interval_ms = v;
                    }
                }
                "hold-ms" => {
                    let (lo, hi) = ORACLE_EYES_HOLD_MS;
                    if let Some(v) = self.int_in(n, lo, hi) {
                        self.oracle_eyes.hold_ms = v;
                    }
                }
                "debug" => {
                    if let Some(b) = self.flag(n) {
                        self.oracle_eyes.debug = b;
                    }
                }
                "bind" => self.apply_oracle_eyes_bind(n),
                _ => self.unknown_key(n, "oracle-eyes", "oracle-eyes key"),
            }
        }
    }

    pub(crate) fn apply_wallpaper(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() == "output" {
                self.apply_wallpaper_output(n);
                continue;
            }
            match self.wallpaper_key(n, "wallpaper") {
                Some(WallpaperKey::Path(p)) => self.wallpaper.path = Some(p),
                Some(WallpaperKey::Mode(m)) => self.wallpaper.mode = m,
                Some(WallpaperKey::Color(c)) => self.wallpaper.color = c,
                None => {}
            }
        }
    }

    /// `wallpaper { output "<name>" { .. } }`: any subset of the global keys.
    /// A later block for the same name overrides key by key.
    pub(crate) fn apply_wallpaper_output(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string) else {
            self.reject(
                node,
                "wallpaper.output needs an output name, e.g. output \"DP-1\" { … }",
            );
            return;
        };
        let mut o = self
            .wallpaper
            .outputs
            .iter()
            .find(|o| o.name == name)
            .cloned()
            .unwrap_or_else(|| WallpaperOutput {
                name: name.to_owned(),
                ..WallpaperOutput::default()
            });
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            match self.wallpaper_key(n, "wallpaper.output") {
                Some(WallpaperKey::Path(p)) => o.path = Some(p),
                Some(WallpaperKey::Mode(m)) => o.mode = Some(m),
                Some(WallpaperKey::Color(c)) => o.color = Some(c),
                None => {}
            }
        }
        match self.wallpaper.outputs.iter_mut().find(|x| x.name == o.name) {
            Some(slot) => *slot = o,
            None => self.wallpaper.outputs.push(o),
        }
    }

    /// One `path` / `mode` / `color` node, validated; `None` after a refusal.
    pub(crate) fn wallpaper_key(&mut self, n: &KdlNode, prefix: &str) -> Option<WallpaperKey> {
        let name = n.name().value();
        let v = arg(n).and_then(KdlValue::as_string);
        let parsed = match name {
            "path" => v.map(|p| WallpaperKey::Path(p.to_owned())),
            "mode" => v
                .filter(|m| schema::WALLPAPER_MODES.contains(m))
                .map(|m| WallpaperKey::Mode(m.to_owned())),
            "color" => v.and_then(parse_color).map(WallpaperKey::Color),
            _ => {
                if prefix == "wallpaper" {
                    self.unknown_key(n, prefix, "wallpaper key");
                } else {
                    self.reject(
                        n,
                        format!("unknown wallpaper.output key {name:?}; expected path, mode or color"),
                    );
                }
                return None;
            }
        };
        if parsed.is_none() {
            let got = arg(n).map_or_else(|| "nothing".to_owned(), |v| v.to_string());
            let msg = match name {
                "path" => format!("{prefix}.path needs a string, got {got}"),
                "mode" => format!(
                    "{prefix}.mode: unknown value {got}; expected one of {}",
                    schema::WALLPAPER_MODES.join(", ")
                ),
                _ => format!("{prefix}.color: {got} is not a colour; expected \"#rrggbb\""),
            };
            self.reject(n, msg);
        }
        parsed
    }

    pub(crate) fn apply_setup(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "profile" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) if schema::SETUP_PROFILES.contains(&v) => self.setup.profile = v.to_owned(),
                    other => self.reject(
                        n,
                        format!(
                            "unknown setup profile (other={other:?}); expected one of {}",
                            schema::SETUP_PROFILES.join(", ")
                        ),
                    ),
                },
                "complete" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(b) => self.setup.complete = b,
                    None => self.reject(n, "complete expects #true or #false"),
                },
                "pending-preset" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(b) => self.setup.pending_preset = b,
                    None => self.reject(n, "pending-preset expects #true or #false"),
                },
                _ => self.unknown_key(n, "setup", "setup key"),
            }
        }
    }

    pub(crate) fn apply_workspace(&mut self, node: &KdlNode) {
        let Some(idx) = arg(node).and_then(KdlValue::as_integer) else {
            self.reject(node, "workspace node needs a number argument");
            return;
        };
        if !(1..=10).contains(&idx) {
            self.reject(node, format!("workspace out of range 1..=10 (idx={})", idx));
            return;
        }
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() == "layout" {
                let slot = &mut self.workspace_layout[idx as usize - 1];
                match arg(n).and_then(KdlValue::as_string) {
                    Some("radiant") => *slot = Some(LayoutKind::Radiant),
                    Some("dwindle") => *slot = Some(LayoutKind::Dwindle),
                    Some("master") => *slot = Some(LayoutKind::Master),
                    other => self.reject(n, format!("unknown workspace layout {other:?}")),
                }
            }
        }
    }

    pub(crate) fn apply_output(&mut self, node: &KdlNode) {
        let Some(pattern) = arg(node).and_then(KdlValue::as_string).map(str::to_owned) else {
            self.reject(node, "output node needs a name pattern argument");
            return;
        };
        let mut rule = OutputRule {
            pattern,
            ..Default::default()
        };
        let Some(children) = node.children() else {
            self.outputs.push(rule);
            return;
        };
        for n in children.nodes() {
            let name = n.name().value();
            if schema::find(schema::OUTPUT_KEYS, name).is_none() {
                self.reject(n, format!("unknown output key {name:?}"));
                continue;
            }
            match name {
                "mode" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(crate::outputs::parse_mode)
                {
                    Some(m) => rule.mode = Some(m),
                    None => self.reject(n, "bad output mode, expected \"1920x1080@60\""),
                },
                "position" => {
                    let a = n.entries();
                    match (
                        a.first().and_then(|e| e.value().as_integer()),
                        a.get(1).and_then(|e| e.value().as_integer()),
                    ) {
                        (Some(x), Some(y)) => rule.position = Some((x as i32, y as i32)),
                        _ => self.reject(n, "output position takes two integers"),
                    }
                }
                "scale" => match arg(n).and_then(as_f64) {
                    Some(s) if s > 0.0 => rule.scale = Some(s),
                    _ => self.reject(n, "output scale must be a positive number"),
                },
                "transform" => match arg(n).and_then(KdlValue::as_string) {
                    Some(t) if crate::outputs::is_transform_name(t) => rule.transform = Some(t.to_owned()),
                    other => self.reject(n, format!("unknown output transform {other:?}")),
                },
                "enabled" | "disabled" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(v) => rule.enabled = Some(v == (name == "enabled")),
                    None => rule.enabled = Some(name == "enabled"),
                },
                "lid-close" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) if schema::LID_CLOSE.contains(&v) => rule.lid_close = Some(v.to_owned()),
                    _ => self.reject(n, "output lid-close must be off, suspend or ignore"),
                },
                // `overscan 30` for all four edges, or any subset of
                // `overscan top=20 bottom=20 left=40 right=40`.
                "overscan" => {
                    let has_value = n.entries().iter().any(|e| e.value().as_integer().is_some());
                    let negative = n
                        .entries()
                        .iter()
                        .any(|e| e.value().as_integer().is_some_and(|i| i < 0));
                    if !has_value || negative {
                        self.reject(
                            n,
                            "output overscan takes non-negative pixels: `overscan 30` or \
                             `overscan top=20 left=40`",
                        );
                    } else {
                        rule.overscan = Some(crate::outputs::overscan_of(n));
                    }
                }
                "vrr" | "adaptive-sync" => rule.vrr = arg(n).and_then(KdlValue::as_bool).or(Some(true)),
                "number" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=255).contains(&v) => rule.number = Some(v as u8),
                    _ => self.reject(n, "output number must be between 1 and 255"),
                },
                other => self.reject(n, format!("unknown output key {other:?}")),
            }
        }
        self.outputs.push(rule);
    }
}
