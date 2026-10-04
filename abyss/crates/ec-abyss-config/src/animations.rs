// SPDX-License-Identifier: AGPL-3.0-only
//! `animations { … }`: presets, per-event overrides and their resolution
//! (COMP-13 §1.1, COMP-02 §9).
//!
//! KDL is the source of truth. A preset expands here, once, into a style,
//! duration and curve per [`Event`]; the compositor, Settings and the taskbar
//! all call [`Animations::resolve`] rather than keeping their own tables.
//!
//! Animation is render-only and never reaches an agent: `scene` and
//! `get_tree` always report target geometry, never an interpolated one
//! (COMP-08). Nothing in this module changes that; it only says how long and
//! in what shape a frame-to-frame transition is drawn.

use std::collections::BTreeMap;

/// Everything that can animate. Each is a fixed child node of `animations`,
/// so its keys are plain dotted paths (`animations.window-open.style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Event {
    WindowOpen,
    WindowClose,
    WindowMove,
    WorkspaceSwitch,
    WindowToWorkspace,
    Minimize,
    Unminimize,
    Fullscreen,
    LayerOpen,
    LayerClose,
    Focus,
    ChipAdd,
    ChipRemove,
    BarLayout,
    Toast,
}

impl Event {
    pub const ALL: [Event; 15] = [
        Event::WindowOpen,
        Event::WindowClose,
        Event::WindowMove,
        Event::WorkspaceSwitch,
        Event::WindowToWorkspace,
        Event::Minimize,
        Event::Unminimize,
        Event::Fullscreen,
        Event::LayerOpen,
        Event::LayerClose,
        Event::Focus,
        Event::ChipAdd,
        Event::ChipRemove,
        Event::BarLayout,
        Event::Toast,
    ];

    /// The KDL node name.
    pub fn key(self) -> &'static str {
        match self {
            Event::WindowOpen => "window-open",
            Event::WindowClose => "window-close",
            Event::WindowMove => "window-move",
            Event::WorkspaceSwitch => "workspace-switch",
            Event::WindowToWorkspace => "window-to-workspace",
            Event::Minimize => "minimize",
            Event::Unminimize => "unminimize",
            Event::Fullscreen => "fullscreen",
            Event::LayerOpen => "layer-open",
            Event::LayerClose => "layer-close",
            Event::Focus => "focus",
            Event::ChipAdd => "chip-add",
            Event::ChipRemove => "chip-remove",
            Event::BarLayout => "bar-layout",
            Event::Toast => "toast",
        }
    }

    pub fn parse(s: &str) -> Option<Event> {
        Event::ALL.into_iter().find(|e| e.key() == s)
    }

    /// The built-in styles, cheap 2D only (offset, scale, alpha). The first is
    /// the Smooth preset's; the last is always `none`. An add-on style
    /// (`pack:style`) is never in this list.
    pub fn styles(self) -> &'static [&'static str] {
        match self {
            Event::WindowOpen => &["pop", "fade", "slide", "zoom", "none"],
            Event::WindowClose => &["pop", "fade", "slide", "none"],
            Event::WindowMove => &["glide", "morph", "none"],
            Event::WorkspaceSwitch => &["slide", "slide-vertical", "fade", "none"],
            Event::WindowToWorkspace => &["carry", "fade", "none"],
            Event::Minimize | Event::Unminimize => &["shrink", "fade", "none"],
            Event::Fullscreen => &["morph", "fade", "none"],
            Event::LayerOpen | Event::LayerClose => &["slide", "fade", "pop", "none"],
            Event::Focus => &["crossfade", "none"],
            Event::ChipAdd | Event::ChipRemove => &["grow", "fade", "none"],
            Event::BarLayout => &["glide", "none"],
            Event::Toast => &["slide", "fade", "none"],
        }
    }

    /// Whether `style` is one of this event's built-in styles.
    pub fn is_builtin(self, style: &str) -> bool {
        self.styles().contains(&style)
    }
}

/// Whether `style` names an add-on style: `pack:style`, both halves
/// non-empty and made of `[a-z0-9_-]`. Stored without checking the pack is
/// installed; a missing one falls back at render time, not at parse time.
pub fn is_addon_style(style: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    };
    style.split_once(':').is_some_and(|(p, s)| ok(p) && ok(s))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    Off,
    Subtle,
    Smooth,
    Lively,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Off, Preset::Subtle, Preset::Smooth, Preset::Lively];

    pub fn key(self) -> &'static str {
        match self {
            Preset::Off => "off",
            Preset::Subtle => "subtle",
            Preset::Smooth => "smooth",
            Preset::Lively => "lively",
        }
    }

    pub fn parse(s: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.key() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// Critically damped: carries velocity through a retarget.
    Spring,
    /// Underdamped: overshoots and settles.
    Bounce,
}

impl Curve {
    pub const ALL: [Curve; 6] = [
        Curve::Linear,
        Curve::EaseIn,
        Curve::EaseOut,
        Curve::EaseInOut,
        Curve::Spring,
        Curve::Bounce,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::EaseIn => "ease-in",
            Curve::EaseOut => "ease-out",
            Curve::EaseInOut => "ease-in-out",
            Curve::Spring => "spring",
            Curve::Bounce => "bounce",
        }
    }

    pub fn parse(s: &str) -> Option<Curve> {
        Curve::ALL.into_iter().find(|c| c.key() == s)
    }
}

/// One event's per-event override, as written in KDL. Each field is optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Override {
    pub style: Option<String>,
    pub duration_ms: Option<u32>,
    pub curve: Option<Curve>,
}

impl Override {
    pub fn is_empty(&self) -> bool {
        self.style.is_none() && self.duration_ms.is_none() && self.curve.is_none()
    }
}

/// What the engine consumes. `style == "none"` (or `duration_ms == 0`) means:
/// don't animate.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub style: String,
    pub duration_ms: u32,
    pub curve: Curve,
}

impl Resolved {
    pub fn off(&self) -> bool {
        self.style == "none" || self.duration_ms == 0
    }
}

/// `animations { preset …; speed …; reduce-motion …; <event> { … } }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Animations {
    pub preset: Preset,
    /// Divides every duration; `0.25..=4.0`.
    pub speed: f64,
    pub reduce_motion: bool,
    pub overrides: BTreeMap<Event, Override>,
}

impl Default for Animations {
    fn default() -> Self {
        Self {
            preset: Preset::Smooth,
            speed: 1.0,
            reduce_motion: false,
            overrides: BTreeMap::new(),
        }
    }
}

/// Smooth's row for `ev`: the base every preset is written against.
fn smooth(ev: Event) -> (&'static str, u32, Curve) {
    use Curve::*;
    match ev {
        Event::WindowOpen => ("pop", 220, Spring),
        Event::WindowClose => ("pop", 180, EaseOut),
        Event::WindowMove => ("glide", 220, Spring),
        Event::WorkspaceSwitch => ("slide", 250, Spring),
        Event::WindowToWorkspace => ("carry", 250, Spring),
        Event::Minimize | Event::Unminimize => ("shrink", 220, EaseInOut),
        Event::Fullscreen => ("morph", 220, Spring),
        Event::LayerOpen => ("slide", 200, EaseOut),
        Event::LayerClose => ("slide", 160, EaseOut),
        Event::Focus => ("crossfade", 150, EaseOut),
        Event::ChipAdd | Event::ChipRemove => ("grow", 220, Spring),
        Event::BarLayout => ("glide", 220, Spring),
        Event::Toast => ("slide", 220, Spring),
    }
}

/// The preset's row for `ev`, before any override, speed or reduce-motion.
/// `Off` keeps Smooth's duration and curve so an override that names only a
/// style still has something sensible to run at.
fn preset_row(preset: Preset, ev: Event) -> (&'static str, u32, Curve) {
    let (style, ms, curve) = smooth(ev);
    match preset {
        Preset::Smooth => (style, ms, curve),
        Preset::Off => ("none", ms, curve),
        Preset::Subtle => match ev {
            Event::WindowMove => ("glide", 120, Curve::EaseOut),
            Event::Focus => ("crossfade", 120, Curve::EaseOut),
            Event::BarLayout => ("glide", 120, Curve::EaseOut),
            Event::WorkspaceSwitch | Event::WindowToWorkspace => ("fade", 150, Curve::EaseOut),
            _ if ev.is_builtin("fade") => ("fade", 120, Curve::EaseOut),
            _ => (style, 120, Curve::EaseOut),
        },
        Preset::Lively => match ev {
            Event::WindowOpen => ("zoom", 320, Curve::Bounce),
            Event::WindowMove => ("morph", 280, Curve::Bounce),
            Event::WorkspaceSwitch => ("slide", 300, Curve::Bounce),
            Event::ChipAdd | Event::ChipRemove => ("grow", 280, Curve::Bounce),
            Event::Toast => ("slide", 280, Curve::Bounce),
            _ => (style, (ms as f64 * 1.2).round() as u32, curve),
        },
    }
}

impl Animations {
    /// Preset, then the event's override, then `speed`, then `reduce_motion`.
    /// An add-on style passes through untouched; the engine falls back to
    /// [`Event::styles`]`[0]` for any style it does not know.
    pub fn resolve(&self, ev: Event) -> Resolved {
        let (style, ms, curve) = preset_row(self.preset, ev);
        let o = self.overrides.get(&ev);
        let style = o
            .and_then(|o| o.style.clone())
            .unwrap_or_else(|| style.to_owned());
        let ms = o.and_then(|o| o.duration_ms).unwrap_or(ms);
        let curve = o.and_then(|o| o.curve).unwrap_or(curve);
        let speed = if self.speed > 0.0 { self.speed } else { 1.0 };
        let mut r = Resolved {
            style,
            duration_ms: (ms as f64 / speed).round() as u32,
            curve,
        };
        if self.reduce_motion && !r.off() {
            r = match ev {
                Event::WindowMove | Event::WorkspaceSwitch | Event::WindowToWorkspace => Resolved {
                    style: "none".into(),
                    ..r
                },
                _ if ev.is_builtin("fade") => Resolved {
                    style: "fade".into(),
                    duration_ms: r.duration_ms.min(100),
                    curve: Curve::EaseOut,
                },
                _ => Resolved {
                    style: "none".into(),
                    ..r
                },
            };
        }
        r
    }

    /// Whether any event carries an override. Derived, never stored: a
    /// preset card in Settings clears every override in the same write.
    pub fn custom(&self) -> bool {
        !self.overrides.is_empty()
    }

    /// Whether overlays without an event of their own (the annotation HUD)
    /// may move. Cheap enough for every frame, unlike [`Self::any`].
    pub fn motion(&self) -> bool {
        self.preset != Preset::Off && !self.reduce_motion
    }

    /// False when every event resolves to "don't animate".
    pub fn any(&self) -> bool {
        Event::ALL.into_iter().any(|ev| !self.resolve(ev).off())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(preset: Preset) -> Animations {
        Animations {
            preset,
            ..Animations::default()
        }
    }

    fn row(a: &Animations, ev: Event) -> (String, u32, Curve) {
        let r = a.resolve(ev);
        (r.style, r.duration_ms, r.curve)
    }

    #[test]
    fn names_round_trip_and_style_lists_are_well_formed() {
        for ev in Event::ALL {
            assert_eq!(Event::parse(ev.key()), Some(ev));
            let s = ev.styles();
            assert_eq!(s.last(), Some(&"none"), "{}", ev.key());
            assert_eq!(s[0], smooth(ev).0, "{}: first style is Smooth's", ev.key());
        }
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.key()), Some(p));
        }
        for c in Curve::ALL {
            assert_eq!(Curve::parse(c.key()), Some(c));
        }
        assert_eq!(Event::parse("windows"), None);
    }

    #[test]
    fn off_resolves_every_event_to_none() {
        let a = with(Preset::Off);
        for ev in Event::ALL {
            assert!(a.resolve(ev).off(), "{}", ev.key());
        }
        assert!(!a.any());
    }

    #[test]
    fn smooth_is_the_default() {
        let a = Animations::default();
        assert_eq!(a.preset, Preset::Smooth);
        assert!(a.any() && !a.custom());
    }

    #[test]
    fn smooth_is_the_table() {
        use Curve::*;
        let a = with(Preset::Smooth);
        let want = [
            (Event::WindowOpen, "pop", 220, Spring),
            (Event::WindowClose, "pop", 180, EaseOut),
            (Event::WindowMove, "glide", 220, Spring),
            (Event::WorkspaceSwitch, "slide", 250, Spring),
            (Event::WindowToWorkspace, "carry", 250, Spring),
            (Event::Minimize, "shrink", 220, EaseInOut),
            (Event::Unminimize, "shrink", 220, EaseInOut),
            (Event::Fullscreen, "morph", 220, Spring),
            (Event::LayerOpen, "slide", 200, EaseOut),
            (Event::LayerClose, "slide", 160, EaseOut),
            (Event::Focus, "crossfade", 150, EaseOut),
            (Event::ChipAdd, "grow", 220, Spring),
            (Event::ChipRemove, "grow", 220, Spring),
            (Event::BarLayout, "glide", 220, Spring),
            (Event::Toast, "slide", 220, Spring),
        ];
        for (ev, s, ms, c) in want {
            assert_eq!(row(&a, ev), (s.to_string(), ms, c), "{}", ev.key());
        }
        assert!(a.any());
        assert!(!a.custom());
    }

    #[test]
    fn subtle_fades_where_it_can() {
        let a = with(Preset::Subtle);
        for ev in Event::ALL {
            let (s, ms, c) = row(&a, ev);
            assert_eq!(c, Curve::EaseOut, "{}", ev.key());
            let want = match ev {
                Event::WindowMove | Event::BarLayout => ("glide", 120),
                Event::Focus => ("crossfade", 120),
                Event::WorkspaceSwitch | Event::WindowToWorkspace => ("fade", 150),
                _ => ("fade", 120),
            };
            assert_eq!((s.as_str(), ms), want, "{}", ev.key());
        }
    }

    #[test]
    fn lively_bounces_and_stretches_the_rest() {
        let a = with(Preset::Lively);
        assert_eq!(row(&a, Event::WindowOpen), ("zoom".into(), 320, Curve::Bounce));
        assert_eq!(row(&a, Event::WindowMove), ("morph".into(), 280, Curve::Bounce));
        assert_eq!(
            row(&a, Event::WorkspaceSwitch),
            ("slide".into(), 300, Curve::Bounce)
        );
        assert_eq!(row(&a, Event::ChipAdd), ("grow".into(), 280, Curve::Bounce));
        assert_eq!(row(&a, Event::ChipRemove), ("grow".into(), 280, Curve::Bounce));
        assert_eq!(row(&a, Event::Toast), ("slide".into(), 280, Curve::Bounce));
        // Everything else is Smooth's style and curve at 1.2x.
        assert_eq!(row(&a, Event::WindowClose), ("pop".into(), 216, Curve::EaseOut));
        assert_eq!(row(&a, Event::Focus), ("crossfade".into(), 180, Curve::EaseOut));
        assert_eq!(row(&a, Event::LayerClose), ("slide".into(), 192, Curve::EaseOut));
        assert_eq!(
            row(&a, Event::WindowToWorkspace),
            ("carry".into(), 300, Curve::Spring)
        );
    }

    #[test]
    fn an_override_wins_field_by_field() {
        let mut a = with(Preset::Smooth);
        a.overrides.insert(
            Event::WindowOpen,
            Override {
                style: Some("fade".into()),
                duration_ms: None,
                curve: Some(Curve::Linear),
            },
        );
        assert_eq!(row(&a, Event::WindowOpen), ("fade".into(), 220, Curve::Linear));
        assert!(a.custom());
        // Other events are untouched.
        assert_eq!(row(&a, Event::WindowClose), ("pop".into(), 180, Curve::EaseOut));
        // Under Off, an override that names a style animates that one event
        // at Smooth's timing.
        let mut off = with(Preset::Off);
        off.overrides.insert(
            Event::WindowMove,
            Override {
                style: Some("glide".into()),
                ..Override::default()
            },
        );
        assert_eq!(row(&off, Event::WindowMove), ("glide".into(), 220, Curve::Spring));
        assert!(off.any());
        // An override of only a duration does not switch an Off event on.
        let mut off = with(Preset::Off);
        off.overrides.insert(
            Event::Focus,
            Override {
                duration_ms: Some(300),
                ..Override::default()
            },
        );
        assert!(off.resolve(Event::Focus).off());
        // A zero duration is off whatever the style.
        a.overrides.insert(
            Event::Toast,
            Override {
                duration_ms: Some(0),
                ..Override::default()
            },
        );
        assert!(a.resolve(Event::Toast).off());
    }

    #[test]
    fn speed_divides_durations() {
        let mut a = with(Preset::Smooth);
        a.speed = 2.0;
        assert_eq!(a.resolve(Event::WindowOpen).duration_ms, 110);
        a.speed = 0.25;
        assert_eq!(a.resolve(Event::WindowOpen).duration_ms, 880);
        a.speed = 3.0;
        // 180 / 3, and 220 / 3 rounds to 73.
        assert_eq!(a.resolve(Event::WindowClose).duration_ms, 60);
        assert_eq!(a.resolve(Event::WindowOpen).duration_ms, 73);
        // An overridden duration is scaled too.
        a.overrides.insert(
            Event::Focus,
            Override {
                duration_ms: Some(300),
                ..Override::default()
            },
        );
        assert_eq!(a.resolve(Event::Focus).duration_ms, 100);
    }

    #[test]
    fn reduce_motion_fades_short_or_stops() {
        let mut a = with(Preset::Lively);
        a.reduce_motion = true;
        for ev in [
            Event::WindowMove,
            Event::WorkspaceSwitch,
            Event::WindowToWorkspace,
        ] {
            assert!(a.resolve(ev).off(), "{}", ev.key());
        }
        assert_eq!(row(&a, Event::WindowOpen), ("fade".into(), 100, Curve::EaseOut));
        assert_eq!(row(&a, Event::Toast), ("fade".into(), 100, Curve::EaseOut));
        // No fade on offer: off.
        assert!(a.resolve(Event::Focus).off());
        assert!(a.resolve(Event::BarLayout).off());
        // A duration already under 100 is kept, after speed.
        a.speed = 4.0;
        assert_eq!(a.resolve(Event::LayerClose).duration_ms, 48);
        // Reduce motion never switches an Off event on.
        let mut off = with(Preset::Off);
        off.reduce_motion = true;
        assert!(!off.any());
    }

    #[test]
    fn an_addon_style_passes_through() {
        assert!(is_addon_style("ec-anim-pack:embers"));
        for bad in ["embers", ":x", "x:", "A:b", "a:b c", "a:b:c"] {
            assert!(!is_addon_style(bad), "{bad}");
        }
        let mut a = with(Preset::Smooth);
        a.overrides.insert(
            Event::WindowClose,
            Override {
                style: Some("ec-anim-pack:embers".into()),
                ..Override::default()
            },
        );
        let r = a.resolve(Event::WindowClose);
        assert_eq!(r.style, "ec-anim-pack:embers");
        assert_eq!((r.duration_ms, r.curve), (180, Curve::EaseOut));
        // The engine's fallback for a style it does not know is the first
        // built-in, which is never `none`.
        assert!(!Event::WindowClose.is_builtin(&r.style));
        assert_ne!(Event::WindowClose.styles()[0], "none");
    }
}
