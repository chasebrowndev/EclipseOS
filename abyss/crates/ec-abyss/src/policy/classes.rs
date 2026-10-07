// SPDX-License-Identifier: AGPL-3.0-only
//! Window classes and app trust, as abyss holds them (S-05 §3, §5, §6, §8).
//! TCB.
//!
//! The class itself is `ec_policy_eval::classify::classify`, over facts read
//! here. What this module adds is time (S-05 §5):
//!
//! - **Raises are immediate.** [`class_of`] always computes the current
//!   class; if it is higher than what is held, it wins at once.
//! - **Downgrades wait.** A lower class takes effect only after the raising
//!   condition has been continuously absent for [`DOWNGRADE_GRACE`].
//!   [`recompute`] runs on map and on every commit (title and app_id
//!   changes arrive that way), so a title that flickers back to its raising
//!   value restarts the wait. A reader in the gap still sees the held class.
//!
//! With no table, everything is `secret` (S-05 §8). The compositor's own
//! raise (the sensitive set, `capture.redact-app-id`) feeds `classify` as a
//! raise and never lowers. An X11 window a rule tries to make `secret` is
//! refused, logged, and stays `private` (COMP-07 §2).
//!
//! No allocation for a Wayland window: app_id and title are borrowed under
//! the surface lock. (An X11 window's are copied out of the X11 surface.)

use std::time::{Duration, Instant};

use ec_policy_eval::check::Trust;
use ec_policy_eval::classify::{self, Classified, TargetFacts};
use ec_policy_eval::Class;
use smithay::desktop::{Window, WindowSurface};
use smithay::utils::IsAlive;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::state::AbyssState;

/// S-02 `downgrade_grace` default (S-05 §5 rule 2).
pub const DOWNGRADE_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy)]
struct Held {
    class: Class,
    /// When the raising condition was first seen gone, if it is.
    absent_since: Option<Instant>,
}

/// Held classes, one per window that has been classified.
#[derive(Debug, Default)]
pub struct Classes {
    held: Vec<(Window, Held)>,
}

/// The connector name of the output `window` is on.
fn output_of<'a>(state: &'a AbyssState, window: &Window) -> Option<&'a str> {
    state
        .outputs
        .iter()
        .find(|e| {
            e.workspaces
                .iter()
                .any(|ws| ws.all_windows().iter().any(|w| w == window))
        })
        .map(|e| e.connector.as_str())
}

/// The compositor's own raise: its sensitive set or `capture.redact-app-id`.
fn own_raise(state: &AbyssState, window: &Window, app_id: Option<&str>) -> Class {
    let in_set = crate::shell::window_surface(window).is_some_and(|s| state.sensitive.contains(&s));
    let redacted = app_id.is_some_and(|id| state.config.capture.redact_app_id.iter().any(|r| r == id));
    if in_set || redacted {
        Class::Secret
    } else {
        Class::Public
    }
}

/// Run `f` on `window`'s facts.
fn with_target<R>(state: &AbyssState, window: &Window, f: impl FnOnce(&TargetFacts<'_>) -> R) -> R {
    let output = output_of(state, window);
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => with_states(t.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>();
            let guard = data.and_then(|d| d.lock().ok());
            let (app_id, title) = match &guard {
                Some(d) => (d.app_id.as_deref(), d.title.as_deref()),
                None => (None, None),
            };
            f(&TargetFacts {
                app_id,
                title,
                url: None,
                output,
                launched_by: None,
                xwayland: false,
            })
        }),
        WindowSurface::X11(s) => {
            let (app_id, title) = (s.class(), s.title());
            f(&TargetFacts {
                app_id: Some(&app_id),
                title: Some(&title),
                url: None,
                output,
                launched_by: None,
                xwayland: true,
            })
        }
    }
}

/// The class `window` would have right now, before any held raise.
fn fresh(state: &AbyssState, window: &Window) -> Classified {
    let Some(table) = state.policy_table.as_ref() else {
        return Classified {
            class: Class::Secret,
            x11_secret_refused: false,
        };
    };
    with_target(state, window, |t| {
        let raise = own_raise(state, window, t.app_id);
        classify::classify(&table.classifier, t, raise)
    })
}

/// The class agents see `window` as (S-05 §5): the current class, or a
/// higher held one still inside its downgrade grace.
pub fn class_of(state: &AbyssState, window: &Window) -> Class {
    if !window.alive() {
        return Class::Secret;
    }
    let now = fresh(state, window).class;
    match state.classes.held.iter().find(|(w, _)| w == window) {
        Some((_, h)) if now < h.class => match h.absent_since {
            Some(t) if t.elapsed() >= DOWNGRADE_GRACE => now,
            _ => h.class,
        },
        _ => now,
    }
}

/// Re-classify `window` (on map and on commit), updating what is held.
pub fn recompute(state: &mut AbyssState, window: &Window) {
    state.classes.held.retain(|(w, _)| w.alive());
    let f = fresh(state, window);
    if f.x11_secret_refused {
        tracing::warn!("an X11 window matched a secret classify rule; X11 windows cannot be secret (COMP-07 §2), kept private");
    }
    let entry = state.classes.held.iter_mut().find(|(w, _)| w == window);
    match entry {
        None => state.classes.held.push((
            window.clone(),
            Held {
                class: f.class,
                absent_since: None,
            },
        )),
        Some((_, h)) if f.class >= h.class => {
            h.class = f.class;
            h.absent_since = None;
        }
        Some((_, h)) => match h.absent_since {
            None => h.absent_since = Some(Instant::now()),
            Some(t) if t.elapsed() >= DOWNGRADE_GRACE => {
                h.class = f.class;
                h.absent_since = None;
            }
            Some(_) => {}
        },
    }
}

/// The app trust of `window` (S-05 §6). With no table every app is
/// `untrusted` (S-05 §8).
pub fn trust_of(state: &AbyssState, window: &Window) -> Trust {
    let Some(table) = state.policy_table.as_ref() else {
        return Trust::Untrusted;
    };
    with_target(state, window, |t| classify::trust(&table.classifier, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_is_the_spec_default() {
        assert_eq!(DOWNGRADE_GRACE, Duration::from_millis(500));
    }
}
