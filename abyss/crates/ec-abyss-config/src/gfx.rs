// SPDX-License-Identifier: AGPL-3.0-only
//! Graphics profiles: `decoration.profile "auto|minimal|low|balanced|full"`.
//!
//! A profile sets the *defaults* of the visual keys (rounding, blur, shadow,
//! animations). Precedence is explicit key in `abyss.kdl` > profile > built-in
//! default: a key the file writes is recorded in [`GfxConfig::explicit`] and
//! the profile never touches it. `full` equals the built-in defaults, so a
//! machine on `full` renders exactly as before this feature existed.
//!
//! `auto` follows the profile the compositor detected at startup from the
//! renderer, RAM, CPU count, output size and virtual-machine hints (a pure
//! decision in `ec-abyss`), then any session step-down by the frame-time
//! governor. Both reach this crate through two process-wide atomics, because a
//! reload re-reads the files with no other channel to the compositor. Nothing
//! here is ever written back to a file.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::animations::Preset;
use crate::decoration::{BlurMode, Decoration};

/// How much the compositor draws. Ordered: a higher profile costs more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GfxProfile {
    /// No blur, no shadow, no animation, square corners; scanout friendly.
    Minimal,
    /// Frosted backdrop with a short chain, light shadow, subtle animation.
    Low,
    /// Glass with a shorter chain than `full`.
    Balanced,
    /// Today's glass defaults.
    Full,
}

impl GfxProfile {
    /// Lightest first, the order a chooser shows them.
    pub const ALL: [GfxProfile; 4] = [
        GfxProfile::Minimal,
        GfxProfile::Low,
        GfxProfile::Balanced,
        GfxProfile::Full,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Balanced => "balanced",
            Self::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// One line on what the profile changes, for Settings and `ec-ctl`.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Minimal => {
                "No blur, shadows or animation, square corners. Lightest; best on slow or virtual graphics."
            }
            Self::Low => "Frosted backdrop with a short blur chain, light shadows, subtle animation.",
            Self::Balanced => "Full glass look with a shorter blur chain and softer effects.",
            Self::Full => "The full glass look: deep blur, adaptive tint, shadows and spring animation.",
        }
    }

    /// The next lighter profile the governor may fall to. `Full` goes to
    /// `Balanced` and `Balanced` to `Low`; `Low` and `Minimal` never step down
    /// on their own (a weak machine is detected, not discovered by lag).
    pub fn step_down(self) -> Option<Self> {
        match self {
            Self::Full => Some(Self::Balanced),
            Self::Balanced => Some(Self::Low),
            Self::Low | Self::Minimal => None,
        }
    }

    /// The default values this profile gives the keys in [`explicit`].
    pub fn baseline(self) -> Baseline {
        match self {
            Self::Minimal => Baseline {
                rounding: 0,
                blur_mode: BlurMode::Off,
                blur_size: 8,
                blur_passes: 4,
                adaptive: false,
                shadow: false,
                shadow_range: 16,
                animations: Preset::Off,
            },
            Self::Low => Baseline {
                rounding: 9,
                blur_mode: BlurMode::Frost,
                blur_size: 4,
                blur_passes: 2,
                adaptive: false,
                shadow: true,
                shadow_range: 8,
                animations: Preset::Subtle,
            },
            Self::Balanced => Baseline {
                rounding: 9,
                blur_mode: BlurMode::Glass,
                blur_size: 6,
                blur_passes: 3,
                adaptive: false,
                shadow: true,
                shadow_range: 12,
                animations: Preset::Smooth,
            },
            Self::Full => Baseline {
                rounding: 9,
                blur_mode: BlurMode::Glass,
                blur_size: 8,
                blur_passes: 4,
                adaptive: true,
                shadow: true,
                shadow_range: 16,
                animations: Preset::Smooth,
            },
        }
    }
}

/// The visual defaults one profile sets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Baseline {
    pub rounding: i32,
    pub blur_mode: BlurMode,
    pub blur_size: i32,
    pub blur_passes: i32,
    pub adaptive: bool,
    pub shadow: bool,
    pub shadow_range: i32,
    pub animations: Preset,
}

/// Bits of [`GfxConfig::explicit`]: the keys a profile may default.
pub mod explicit {
    pub const ROUNDING: u16 = 1;
    pub const BLUR_MODE: u16 = 1 << 1;
    pub const BLUR_SIZE: u16 = 1 << 2;
    pub const BLUR_PASSES: u16 = 1 << 3;
    pub const ADAPTIVE: u16 = 1 << 4;
    pub const SHADOW: u16 = 1 << 5;
    pub const SHADOW_RANGE: u16 = 1 << 6;
    pub const ANIM_PRESET: u16 = 1 << 7;
}

/// `decoration.profile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileSetting {
    /// Follow detection (and the governor).
    #[default]
    Auto,
    Fixed(GfxProfile),
}

impl ProfileSetting {
    pub const NAMES: &'static [&'static str] = &["auto", "minimal", "low", "balanced", "full"];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed(p) => p.name(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        if s == "auto" {
            Some(Self::Auto)
        } else {
            GfxProfile::parse(s).map(Self::Fixed)
        }
    }
}

/// The profile state a [`crate::Config`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GfxConfig {
    pub setting: ProfileSetting,
    /// `decoration.profile-governor`: allow the session step-down.
    pub governor: bool,
    /// The profile whose baseline was applied on the last resolve.
    pub effective: GfxProfile,
    /// [`explicit`] bits: keys a file wrote, which no profile overrides.
    pub explicit: u16,
}

impl Default for GfxConfig {
    fn default() -> Self {
        Self {
            setting: ProfileSetting::Auto,
            governor: true,
            effective: GfxProfile::Full,
            explicit: 0,
        }
    }
}

// 0 = unset. Stored as `GfxProfile` index + 1. Atomics, not a lock: set at
// startup and by the governor, read by config loads off the hot path.
static DETECTED: AtomicU8 = AtomicU8::new(0);
static CAP: AtomicU8 = AtomicU8::new(0);

fn store(a: &AtomicU8, p: Option<GfxProfile>) {
    a.store(p.map_or(0, |p| p as u8 + 1), Ordering::Relaxed);
}

fn load(a: &AtomicU8) -> Option<GfxProfile> {
    match a.load(Ordering::Relaxed) {
        0 => None,
        n => GfxProfile::ALL.get(n as usize - 1).copied(),
    }
}

/// What the compositor detected; `auto` follows it. Unset means `Full`.
pub fn set_detected(p: Option<GfxProfile>) {
    store(&DETECTED, p);
}

pub fn detected() -> Option<GfxProfile> {
    load(&DETECTED)
}

/// The governor's session ceiling; `auto` never exceeds it. Never persisted.
pub fn set_session_cap(p: Option<GfxProfile>) {
    store(&CAP, p);
}

pub fn session_cap() -> Option<GfxProfile> {
    load(&CAP)
}

/// The profile in force: an explicit choice as written, `auto` as the lower of
/// detection and the session cap (each defaulting to `Full`).
pub fn resolve(setting: ProfileSetting, detected: Option<GfxProfile>, cap: Option<GfxProfile>) -> GfxProfile {
    match setting {
        ProfileSetting::Fixed(p) => p,
        ProfileSetting::Auto => detected
            .unwrap_or(GfxProfile::Full)
            .min(cap.unwrap_or(GfxProfile::Full)),
    }
}

impl crate::Config {
    /// Fill every visual key no file wrote from the effective profile. Runs on
    /// each load and again when detection or the governor moves, so it only
    /// ever touches keys outside [`GfxConfig::explicit`].
    pub fn resolve_gfx_defaults(&mut self) {
        let p = resolve(self.gfx.setting, detected(), session_cap());
        self.apply_profile(p);
    }

    /// [`Config::resolve_gfx_defaults`] for a given profile (the pure core).
    pub fn apply_profile(&mut self, p: GfxProfile) {
        use explicit::*;
        let b = p.baseline();
        let x = self.gfx.explicit;
        let d: &mut Decoration = &mut self.decoration;
        if x & ROUNDING == 0 {
            d.rounding = b.rounding;
        }
        if x & BLUR_MODE == 0 {
            d.blur.mode = b.blur_mode;
        }
        if x & BLUR_SIZE == 0 {
            d.blur.size = b.blur_size;
        }
        if x & BLUR_PASSES == 0 {
            d.blur.passes = b.blur_passes;
        }
        if x & ADAPTIVE == 0 {
            d.blur.adaptive.enabled = b.adaptive;
        }
        if x & SHADOW == 0 {
            d.shadow.enabled = b.shadow;
        }
        if x & SHADOW_RANGE == 0 {
            d.shadow.range = b.shadow_range;
        }
        if x & ANIM_PRESET == 0 {
            self.animations.preset = b.animations;
        }
        self.gfx.effective = p;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_single_for_tests as parse;

    #[test]
    fn full_is_the_builtin_default() {
        let mut c = crate::Config::default();
        let before = (c.decoration.clone(), c.animations.clone());
        c.apply_profile(GfxProfile::Full);
        assert_eq!(c.decoration.rounding, before.0.rounding);
        assert_eq!(c.decoration.blur.mode, before.0.blur.mode);
        assert_eq!(c.decoration.blur.size, before.0.blur.size);
        assert_eq!(c.decoration.blur.passes, before.0.blur.passes);
        assert_eq!(c.decoration.blur.adaptive.enabled, before.0.blur.adaptive.enabled);
        assert_eq!(c.decoration.shadow.enabled, before.0.shadow.enabled);
        assert_eq!(c.decoration.shadow.range, before.0.shadow.range);
        assert_eq!(c.animations, before.1);
    }

    #[test]
    fn profiles_only_get_lighter() {
        let cost = |p: GfxProfile| {
            let b = p.baseline();
            (
                b.blur_mode != BlurMode::Off,
                if b.blur_mode == BlurMode::Off {
                    0
                } else {
                    b.blur_passes
                },
                b.shadow,
                b.animations != Preset::Off,
                b.adaptive,
            )
        };
        for w in GfxProfile::ALL.windows(2) {
            let (lo, hi) = (cost(w[0]), cost(w[1]));
            assert!(lo.0 <= hi.0 && lo.1 <= hi.1 && lo.2 <= hi.2 && lo.3 <= hi.3 && lo.4 <= hi.4);
        }
    }

    #[test]
    fn minimal_has_no_window_effects() {
        let mut c = crate::Config::default();
        c.apply_profile(GfxProfile::Minimal);
        assert!(!c.decoration.any_window_effect(), "scanout stays possible");
        assert_eq!(c.decoration.blur.mode, BlurMode::Off);
        assert!(!c.decoration.shadow.enabled);
        assert_eq!(c.animations.preset, Preset::Off);
    }

    #[test]
    fn explicit_key_beats_profile() {
        let mut c = parse("decoration {\n profile \"minimal\"\n rounding 12\n blur { mode \"frost\" }\n}\n");
        assert_eq!(c.gfx.setting, ProfileSetting::Fixed(GfxProfile::Minimal));
        c.resolve_gfx_defaults();
        assert_eq!(c.gfx.effective, GfxProfile::Minimal);
        assert_eq!(c.decoration.rounding, 12, "written rounding wins");
        assert_eq!(c.decoration.blur.mode, BlurMode::Frost, "written mode wins");
        assert!(
            !c.decoration.shadow.enabled,
            "unwritten shadow follows the profile"
        );
        assert_eq!(c.animations.preset, Preset::Off);
    }

    #[test]
    fn explicit_value_equal_to_a_baseline_still_wins_after_a_profile_change() {
        let mut c = parse("decoration { blur { mode \"glass\" } }\n");
        c.apply_profile(GfxProfile::Minimal);
        assert_eq!(c.decoration.blur.mode, BlurMode::Glass);
        c.apply_profile(GfxProfile::Low);
        assert_eq!(c.decoration.blur.passes, 2);
        assert_eq!(c.decoration.blur.mode, BlurMode::Glass);
    }

    #[test]
    fn legacy_keys_count_as_explicit() {
        let mut c = parse("decoration { blur { enabled #true } }\nanimations { enabled #false }\n");
        c.apply_profile(GfxProfile::Minimal);
        assert_eq!(c.decoration.blur.mode, BlurMode::Blur);
        assert_eq!(c.animations.preset, Preset::Off);
        assert_ne!(c.gfx.explicit & explicit::ANIM_PRESET, 0);
    }

    #[test]
    fn auto_follows_detection_and_the_cap() {
        use GfxProfile::*;
        let auto = ProfileSetting::Auto;
        assert_eq!(resolve(auto, None, None), Full);
        assert_eq!(resolve(auto, Some(Low), None), Low);
        assert_eq!(resolve(auto, Some(Full), Some(Balanced)), Balanced);
        assert_eq!(
            resolve(auto, Some(Minimal), Some(Balanced)),
            Minimal,
            "a cap never raises"
        );
        assert_eq!(
            resolve(ProfileSetting::Fixed(Full), Some(Minimal), Some(Low)),
            Full
        );
    }

    #[test]
    fn bad_profile_is_refused_and_keeps_auto() {
        let c = parse("decoration { profile \"ultra\" }\n");
        assert_eq!(c.gfx.setting, ProfileSetting::Auto);
        assert!(!c.errors.is_empty());
    }

    #[test]
    fn round_trips_through_schema_get() {
        let c = parse("decoration { profile \"low\"; profile-governor #false }\n");
        assert_eq!(
            crate::schema::get(&c, "decoration.profile"),
            Some(crate::schema::Value::Str("low".into()))
        );
        assert!(!c.gfx.governor);
        for n in ProfileSetting::NAMES {
            assert_eq!(ProfileSetting::parse(n).unwrap().name(), *n);
        }
    }

    #[test]
    fn governor_steps_stop_at_low() {
        assert_eq!(GfxProfile::Full.step_down(), Some(GfxProfile::Balanced));
        assert_eq!(GfxProfile::Balanced.step_down(), Some(GfxProfile::Low));
        assert_eq!(GfxProfile::Low.step_down(), None);
    }
}
