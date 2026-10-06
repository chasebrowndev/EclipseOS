// SPDX-License-Identifier: AGPL-3.0-only
//! `Config::apply_*` for the `decoration` and `animations` blocks.

use super::*;

impl Config {
    /// `decoration { rounding 8; active-opacity 1.0; blur { ... } }`.
    /// Out-of-range values are warned about and dropped, keeping the default.
    pub(crate) fn apply_decoration(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "rounding" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=64).contains(&v) => self.decoration.rounding = v as i32,
                    _ => self.reject(n, "decoration rounding must be an integer 0..=64"),
                },
                "active-opacity" | "inactive-opacity" | "dim-inactive" => match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => {
                        let v = v as f32;
                        match name {
                            "active-opacity" => self.decoration.active_opacity = v,
                            "inactive-opacity" => self.decoration.inactive_opacity = v,
                            _ => self.decoration.dim_inactive = v,
                        }
                    }
                    _ => self.reject(n, format!("{name} must be 0.0..=1.0")),
                },
                "blur" => self.apply_blur(n),
                "shadow" => self.apply_shadow(n),
                "glow" => self.apply_glow(n),
                _ => self.unknown_key(n, "decoration", "decoration key"),
            }
        }
    }

    pub(crate) fn apply_blur(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        // An explicit `mode` is the newer word and wins wherever it sits.
        let has_mode = children.nodes().iter().any(|n| n.name().value() == "mode");
        for n in children.nodes() {
            match n.name().value() {
                // Legacy (before `mode`): kept so an old file still loads;
                // `ec-ctl config migrate` rewrites it.
                "enabled" if has_mode => {}
                "enabled" => {
                    self.decoration.blur.mode = if arg(n).and_then(KdlValue::as_bool).unwrap_or(true) {
                        BlurMode::Blur
                    } else {
                        BlurMode::Off
                    };
                }
                "mode" => match arg(n).and_then(KdlValue::as_string).and_then(BlurMode::parse) {
                    Some(m) => self.decoration.blur.mode = m,
                    None => self.reject(n, "blur mode must be \"off\", \"blur\", \"frost\" or \"glass\""),
                },
                "glass" => self.apply_glass(n),
                "frost" => self.apply_frost(n),
                "size" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=64).contains(&v) => self.decoration.blur.size = v as i32,
                    _ => self.reject(n, "blur size must be an integer 1..=64"),
                },
                "passes" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=6).contains(&v) => self.decoration.blur.passes = v as i32,
                    _ => self.reject(n, "blur passes must be an integer 1..=6"),
                },
                _ => self.unknown_key(n, "decoration.blur", "blur key"),
            }
        }
    }

    pub(crate) fn apply_glass(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let glass = &mut self.decoration.blur.glass;
            match n.name().value() {
                "refraction" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=64).contains(&v) => glass.refraction = v as i32,
                    _ => self.reject(n, "glass refraction must be an integer 0..=64"),
                },
                "bevel" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=128).contains(&v) => glass.bevel = v as i32,
                    _ => self.reject(n, "glass bevel must be an integer 1..=128"),
                },
                "dispersion" => match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => glass.dispersion = v as f32,
                    _ => self.reject(n, "glass dispersion must be 0.0..=1.0"),
                },
                "rim" => match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => glass.rim = v as f32,
                    _ => self.reject(n, "glass rim must be 0.0..=1.0"),
                },
                _ => self.unknown_key(n, "decoration.blur.glass", "glass key"),
            }
        }
    }

    pub(crate) fn apply_frost(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "tint" => match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                    Some(c) => self.decoration.blur.frost.tint = c,
                    None => self.reject(n, "bad color for \"frost tint\""),
                },
                _ => self.unknown_key(n, "decoration.blur.frost", "frost key"),
            }
        }
    }

    pub(crate) fn apply_shadow(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "enabled" => {
                    self.decoration.shadow.enabled = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                "range" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=128).contains(&v) => self.decoration.shadow.range = v as i32,
                    _ => self.reject(n, "shadow range must be an integer 0..=128"),
                },
                _ => self.unknown_key(n, "decoration.shadow", "shadow key"),
            }
        }
    }

    pub(crate) fn apply_glow(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                name @ ("enabled" | "active" | "inactive") => {
                    let v = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                    match name {
                        "enabled" => self.decoration.glow.enabled = v,
                        "active" => self.decoration.glow.active = v,
                        _ => self.decoration.glow.inactive = v,
                    }
                }
                "strength" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=100).contains(&v) => self.decoration.glow.strength = v as i32,
                    _ => self.reject(n, "glow strength must be an integer 0..=100"),
                },
                _ => self.unknown_key(n, "decoration.glow", "glow key"),
            }
        }
    }

    /// `animations { preset "smooth"; speed 1.0; reduce-motion #false;
    /// window-open { style "pop"; duration-ms 220; curve "spring" } }`.
    ///
    /// The legacy forms still parse (`ec-ctl config migrate` rewrites them):
    /// `enabled #false` is `preset "off"` unless this block names a preset;
    /// `animation "<name>" duration=… curve=…` is a full override of its
    /// event, honoured only beside `enabled #true` as it always was, and only
    /// where this block does not write that event the new way.
    pub(crate) fn apply_animations(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut legacy_enabled = None;
        let mut legacy = Vec::new();
        let mut preset_here = false;
        let mut written_here = Vec::new();
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "preset" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(animations::Preset::parse)
                {
                    Some(p) => {
                        self.animations.preset = p;
                        preset_here = true;
                    }
                    None => self.reject(
                        n,
                        format!(
                            "animations preset must be one of {}",
                            schema::ANIMATION_PRESETS.join(", ")
                        ),
                    ),
                },
                "speed" => match arg(n).and_then(as_f64) {
                    Some(v) if (schema::ANIMATION_MIN_SPEED..=schema::ANIMATION_MAX_SPEED).contains(&v) => {
                        self.animations.speed = v;
                    }
                    _ => self.reject(
                        n,
                        format!(
                            "animations speed must be {}..={}",
                            schema::ANIMATION_MIN_SPEED,
                            schema::ANIMATION_MAX_SPEED
                        ),
                    ),
                },
                "reduce-motion" => {
                    if let Some(b) = self.flag(n) {
                        self.animations.reduce_motion = b;
                    }
                }
                "enabled" => legacy_enabled = self.flag(n),
                "animation" => {
                    if let Some(o) = self.legacy_animation(n) {
                        legacy.push(o);
                    }
                }
                _ => match animations::Event::parse(name) {
                    Some(ev) => {
                        written_here.push(ev);
                        self.apply_animation_event(n, ev);
                    }
                    None => self.unknown_key(n, "animations", "animations key"),
                },
            }
        }
        if legacy_enabled == Some(false) && !preset_here {
            self.animations.preset = animations::Preset::Off;
        }
        if legacy_enabled == Some(true) {
            for (ev, o) in legacy {
                if !written_here.contains(&ev) {
                    self.animations.overrides.insert(ev, o);
                }
            }
        }
    }

    /// `window-open { style "pop"; duration-ms 220; curve "spring" }`. One bad
    /// child drops the whole block, so an override never applies in part
    /// (ADR 0064). Fields merge into what earlier files set, key by key.
    pub(crate) fn apply_animation_event(&mut self, node: &KdlNode, ev: animations::Event) {
        if let Some(e) = node.entries().first() {
            self.reject_entry(e, format!("animations.{} takes a block, not arguments", ev.key()));
            return;
        }
        let Some(children) = node.children() else { return };
        let mut o = animations::Override::default();
        let before = self.errors.len();
        for n in children.nodes() {
            match n.name().value() {
                "style" => match arg(n).and_then(KdlValue::as_string) {
                    Some(s) if ev.is_builtin(s) || animations::is_addon_style(s) => o.style = Some(s.to_owned()),
                    other => self.reject(
                        n,
                        format!(
                            "animations.{}.style must be one of {} or an add-on `pack:style` (other={other:?})",
                            ev.key(),
                            ev.styles().join(", ")
                        ),
                    ),
                },
                "duration-ms" => o.duration_ms = self.ms_in(n, 0, schema::ANIMATION_MAX_MS),
                "curve" => match arg(n).and_then(KdlValue::as_string).and_then(animations::Curve::parse) {
                    Some(c) => o.curve = Some(c),
                    None => self.reject(
                        n,
                        format!(
                            "animations.{}.curve must be one of {}",
                            ev.key(),
                            schema::ANIMATION_CURVES.join(", ")
                        ),
                    ),
                },
                _ => self.unknown_key(n, &format!("animations.{}", ev.key()), "animation key"),
            }
        }
        if self.errors.len() != before || o.is_empty() {
            return;
        }
        let into = self.animations.overrides.entry(ev).or_default();
        if o.style.is_some() {
            into.style = o.style;
        }
        if o.duration_ms.is_some() {
            into.duration_ms = o.duration_ms;
        }
        if o.curve.is_some() {
            into.curve = o.curve;
        }
    }

    /// Legacy `animation "windows" duration="150ms" curve="ease-out"`, as the
    /// full override of its event it stands for (see [`schema::LEGACY_ANIMATIONS`]).
    /// A bad name, duration, curve or property drops the node (ADR 0064).
    pub(crate) fn legacy_animation(
        &mut self,
        node: &KdlNode,
    ) -> Option<(animations::Event, animations::Override)> {
        let Some(name) = arg(node).and_then(KdlValue::as_string) else {
            self.reject(node, "animation node needs a name argument");
            return None;
        };
        let Some(&(_, ev, style)) = schema::LEGACY_ANIMATION_MAP.iter().find(|(n, _, _)| *n == name) else {
            self.reject(node, format!("unknown animation name {name:?}"));
            return None;
        };
        let ev = animations::Event::parse(ev).expect("legacy map names a real event");
        let mut duration_ms = schema::ANIMATION_DEFAULT_MS;
        let mut curve = animations::Curve::parse(schema::ANIMATION_DEFAULT_CURVE).expect("default curve");
        for e in node.entries() {
            let Some(key) = e.name().map(|k| k.value().to_owned()) else {
                continue; // the positional name argument
            };
            match key.as_str() {
                "duration" => match parse_duration_ms(e.value()) {
                    Some(ms) if ms <= schema::ANIMATION_MAX_MS => duration_ms = ms,
                    _ => {
                        self.reject(
                            node,
                            format!(
                                "animation duration must be <= 10s, e.g. \"150ms\" (name={})",
                                name
                            ),
                        );
                        return None;
                    }
                },
                // The legacy form never took the motion curves.
                "curve" => match e.value().as_string().filter(|c| EASING_CURVES.contains(c)) {
                    Some(c) => curve = animations::Curve::parse(c).expect("easing is a curve"),
                    other => {
                        self.reject(
                            node,
                            format!("unknown animation curve (name={}, other={:?})", name, other),
                        );
                        return None;
                    }
                },
                // Dropped whole, like a bad duration or curve: a rejected
                // node never applies in part (ADR 0064).
                other => {
                    self.reject(node, format!("unknown animation property {other:?}"));
                    return None;
                }
            }
        }
        Some((
            ev,
            animations::Override {
                style: Some(style.to_owned()),
                duration_ms: Some(duration_ms),
                curve: Some(curve),
            },
        ))
    }
}
