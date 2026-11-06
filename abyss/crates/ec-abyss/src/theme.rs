// SPDX-License-Identifier: AGPL-3.0-only
//! The theme in force (ADR 0084), owned by `AbyssState`.
//!
//! `general.color-scheme` plus the clock decide the effective [`Scheme`]; the
//! scheme picks the theme (`theme-dark` / `theme-light`); the theme's seeds
//! are published over the control socket (`get_theme`, the `theme` event) and
//! applied to what the compositor draws via `Config::apply_appearance`. For
//! `auto` a calloop timer re-evaluates at the next `light-from` / `dark-from`
//! boundary. Nothing here blocks: the clock is read with `localtime_r`.
//!
//! The trusted UI reads [`ThemeState::scheme`] (a later PR); it never sees a
//! colour from here.

use std::time::Duration;

use ec_theme::{Rgba, Scheme};
use serde_json::{json, Value};
use smithay::reexports::calloop::{
    timer::{TimeoutAction, Timer},
    RegistrationToken,
};

use crate::config::{Appearance, ColorSetting};
use crate::state::AbyssState;

/// Longest the `auto` timer sleeps. The monotonic clock stops across suspend
/// and the zone can change, so a boundary is re-checked at least this often.
const MAX_SLEEP: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct ThemeState {
    /// What was last published (and applied).
    appearance: Appearance,
    timer: Option<RegistrationToken>,
}

impl ThemeState {
    /// The effective scheme right now.
    pub fn scheme(&self) -> Scheme {
        self.appearance.scheme
    }

    pub fn appearance(&self) -> &Appearance {
        &self.appearance
    }
}

fn hex(c: Rgba, alpha: bool) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, bl, a] = c.0;
    if alpha {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(r), b(g), b(bl), b(a))
    } else {
        format!("#{:02x}{:02x}{:02x}", b(r), b(g), b(bl))
    }
}

/// The `get_theme` reply, and the payload of the `theme` event.
pub fn payload(state: &AbyssState) -> Value {
    let a = &state.theme.appearance;
    json!({
        "scheme": scheme_name(a.scheme),
        "setting": state.config.general.color_scheme.name(),
        "theme": a.name,
        "seeds": {
            "base": hex(a.seeds.base, false),
            "ink": hex(a.seeds.ink, false),
            "accent": hex(a.seeds.accent, false),
            "accent_text": a.seeds.accent_text.map(|c| hex(c, false)),
            "frost_tint": hex(a.seeds.frost_tint, true),
            "rim": (f64::from(a.seeds.rim) * 1e4).round() / 1e4,
        },
    })
}

fn scheme_name(s: Scheme) -> &'static str {
    match s {
        Scheme::Dark => "dark",
        Scheme::Light => "light",
    }
}

/// Seconds since local midnight.
fn local_second_of_day() -> u32 {
    // SAFETY: `time` with a null pointer only returns the time; `localtime_r`
    // writes only through the pointer we give it.
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm
    };
    (tm.tm_hour as u32 * 3600 + tm.tm_min as u32 * 60 + tm.tm_sec as u32).min(86_399)
}

/// Seconds until the next `light-from` / `dark-from`, from `now`.
fn until_boundary(now: u32, light_from: u16, dark_from: u16) -> u32 {
    [light_from, dark_from]
        .into_iter()
        .map(|m| {
            let at = m as u32 * 60;
            if at > now {
                at - now
            } else {
                at + 86_400 - now
            }
        })
        .min()
        .unwrap_or(86_400)
}

/// Re-evaluate the theme against the live config and the clock: apply it to
/// what the compositor draws, publish a `theme` event if (and only if) the
/// scheme, theme name or seeds changed, and re-arm the `auto` timer. Called on
/// every config apply, at startup and from the timer.
pub fn refresh(state: &mut AbyssState) {
    let now = local_second_of_day();
    let scheme = state.config.scheme_at((now / 60) as u16);
    let next = state.config.appearance_for(scheme);
    state.config.apply_appearance(next.clone());
    // The built-in pointer follows the effective accent; the damage below
    // makes the cursor element re-commit with the rebuilt buffer.
    let [r, g, b, _] = next
        .seeds
        .accent
        .0
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u32);
    ec_abyss_render::cursor::set_accent(r << 16 | g << 8 | b);
    let changed = state.theme.appearance != next;
    state.theme.appearance = next;
    crate::trusted_ui::set_scheme(&mut state.trusted_ui, state.theme.scheme());
    if changed {
        tracing::info!(scheme = scheme_name(scheme), theme = %state.theme.appearance.name, "theme changed");
        let p = payload(state);
        crate::ipc::emit(state, "theme", p);
        crate::backend::damage_all(state);
    }
    arm(state, now);
}

fn arm(state: &mut AbyssState, now: u32) {
    if let Some(t) = state.theme.timer.take() {
        state.loop_handle.remove(t);
    }
    if state.config.general.color_scheme != ColorSetting::Auto {
        return;
    }
    let g = &state.config.general;
    let wait = Duration::from_secs(until_boundary(now, g.light_from, g.dark_from) as u64 + 1).min(MAX_SLEEP);
    let token = state
        .loop_handle
        .insert_source(Timer::from_duration(wait), |_, _, state| {
            // `refresh` re-arms, replacing this source.
            state.theme.timer = None;
            refresh(state);
            TimeoutAction::Drop
        });
    match token {
        Ok(t) => state.theme.timer = Some(t),
        Err(e) => tracing::warn!(%e, "arming the theme timer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_boundary_wraps_midnight() {
        // 06:00 -> light-from 07:00 is an hour away.
        assert_eq!(until_boundary(6 * 3600, 420, 1140), 3600);
        // 20:00 -> next is tomorrow 07:00, 11h.
        assert_eq!(until_boundary(20 * 3600, 420, 1140), 11 * 3600);
        // Exactly on a boundary waits for the other one.
        assert_eq!(until_boundary(7 * 3600, 420, 1140), 12 * 3600);
    }

    #[test]
    fn hex_rounds_channels() {
        assert_eq!(hex(Rgba::rgb(0xf2c33c), false), "#f2c33c");
        assert_eq!(hex(Rgba::rgb(0x17140f), true), "#17140fff");
    }

    #[test]
    fn scheme_change_emits_theme_once_and_get_theme_reports_it() {
        use crate::ipc::gate::Decision;
        use serde_json::json;
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, "").unwrap();
        h.state.config.explicit = Some(file.clone());
        crate::ipc::capture::take();

        // Nothing configured: dark, built-in, no event.
        refresh(&mut h.state);
        assert!(crate::ipc::capture::take().iter().all(|(k, _)| k != "theme"));
        let dark = crate::ipc::methods::dispatch(&mut h.state, 0, "get_theme", &json!({}))
            .ok()
            .unwrap();
        assert_eq!(dark["scheme"], "dark");
        assert_eq!(dark["theme"], "dark");
        assert_eq!(dark["setting"], "dark");
        assert_eq!(dark["seeds"]["base"], "#0b0906");
        assert_eq!(dark["seeds"]["frost_tint"], "#17140fff");

        // Switching to light over the socket emits exactly one `theme`.
        let ok = crate::ipc::config_rpc::dispatch(
            &mut h.state,
            Decision::Allow,
            "set_config_value",
            &json!({"path": "general.color-scheme", "value": "light"}),
        );
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        let events = crate::ipc::capture::take();
        let themes: Vec<_> = events.iter().filter(|(k, _)| k == "theme").collect();
        assert_eq!(themes.len(), 1, "{events:?}");
        assert_eq!(themes[0].1["scheme"], "light");
        assert_eq!(h.state.theme.scheme(), Scheme::Light);
        assert_eq!(themes[0].1["seeds"]["base"], "#f4f1ea");

        // The shared accent: one `theme` event carrying the effective accent
        // and its derived text colour; unsetting it restores the theme's own.
        let ok = crate::ipc::config_rpc::dispatch(
            &mut h.state,
            Decision::Allow,
            "set_config_value",
            &json!({"path": "general.accent", "value": "#1e90ff"}),
        );
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        let events = crate::ipc::capture::take();
        let themes: Vec<_> = events.iter().filter(|(k, _)| k == "theme").collect();
        assert_eq!(themes.len(), 1, "{events:?}");
        assert_eq!(themes[0].1["seeds"]["accent"], "#1e90ff");
        assert!(themes[0].1["seeds"]["accent_text"].is_string());
        let g = crate::ipc::methods::dispatch(&mut h.state, 0, "get_theme", &json!({}))
            .ok()
            .unwrap();
        assert_eq!(g["seeds"]["accent"], "#1e90ff");
        let cfgv = crate::ipc::config_rpc::dispatch(
            &mut h.state,
            Decision::Allow,
            "get_config",
            &json!({"path": "general.accent"}),
        );
        assert!(cfgv.is_ok());
        let ok = crate::ipc::config_rpc::dispatch(
            &mut h.state,
            Decision::Allow,
            "set_config_value",
            &json!({"path": "general.accent", "value": ""}),
        );
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        let events = crate::ipc::capture::take();
        let themes: Vec<_> = events.iter().filter(|(k, _)| k == "theme").collect();
        assert_eq!(themes.len(), 1, "{events:?}");
        assert_eq!(themes[0].1["seeds"]["accent"], "#f2c33c");

        // Re-applying the same config is not a change.
        refresh(&mut h.state);
        assert!(crate::ipc::capture::take().iter().all(|(k, _)| k != "theme"));
        // The compositor draws light: the frost tint followed the theme.
        assert_ne!(
            h.state.config.decoration.blur.frost.tint,
            crate::config::FROST_TINT
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
