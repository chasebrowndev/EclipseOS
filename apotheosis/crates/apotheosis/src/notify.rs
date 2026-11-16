// SPDX-License-Identifier: AGPL-3.0-only
//! Desktop notifications (T-17 §6, AUTOMATION-15).
//!
//! OSC 9 / 777 / 99, `bell.notify` and the `notify` action all end here. The filters
//! (`notifications.when`, the per-sequence toggles, `notifications.max-per-minute`) are pure
//! and decided on the main thread; delivery is one call to `org.freedesktop.Notifications`
//! on the session bus from a worker-pool task, so the main loop never blocks on D-Bus.
//! Titles and bodies are program or user content: delivered, never logged.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use apoth_config::Config;

/// Title and body byte budgets (T-18 §5): longer text is cut at a char boundary.
pub(crate) const TITLE_MAX: usize = 256;
pub(crate) const BODY_MAX: usize = 1024;

/// `notifications.when`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    Unfocused,
    Invisible,
    Always,
    Never,
}

/// The `notifications.*` and `bell.notify` keys.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NotifySettings {
    pub(crate) when: When,
    pub(crate) osc9: bool,
    pub(crate) osc99: bool,
    pub(crate) osc777: bool,
    pub(crate) per_minute: u32,
    /// `notifications.min-duration`: a `notify` run by a `command-finished` rule is dropped
    /// when the command ran for less.
    pub(crate) min_duration: Duration,
    pub(crate) bell: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifySettings {
            when: When::Unfocused,
            osc9: true,
            osc99: true,
            osc777: true,
            per_minute: 30,
            min_duration: Duration::ZERO,
            bell: false,
        }
    }
}

impl NotifySettings {
    pub(crate) fn from_config(c: &Config) -> NotifySettings {
        NotifySettings {
            when: match c.str("notifications.when") {
                Some("invisible") => When::Invisible,
                Some("always") => When::Always,
                Some("never") => When::Never,
                _ => When::Unfocused,
            },
            osc9: c.bool("notifications.osc-9"),
            osc99: c.bool("notifications.osc-99"),
            osc777: c.bool("notifications.osc-777"),
            per_minute: u32::try_from(c.int("notifications.max-per-minute"))
                .unwrap_or(30)
                .clamp(1, 600),
            min_duration: Duration::from_millis(c.millis("notifications.min-duration")),
            bell: c.bool("bell.notify"),
        }
    }

    /// The per-sequence toggle for an OSC number.
    pub(crate) fn accepts(&self, source: u16) -> bool {
        match source {
            9 => self.osc9,
            99 => self.osc99,
            777 => self.osc777,
            _ => false,
        }
    }
}

/// `notifications.when` against the pane's state. `visible` is "the pane is on screen in a
/// window the compositor shows"; without a visibility signal the caller passes `focused`.
pub(crate) fn when_allows(when: When, focused: bool, visible: bool) -> bool {
    match when {
        When::Always => true,
        When::Never => false,
        When::Unfocused => !focused,
        When::Invisible => !visible,
    }
}

/// Per-pane sliding one-minute window.
#[derive(Debug, Default)]
pub(crate) struct RateGate {
    recent: HashMap<u64, VecDeque<Instant>>,
}

impl RateGate {
    /// Admit one notification for `pane` at `now`, at most `per_minute` a minute.
    pub(crate) fn admit(&mut self, pane: u64, per_minute: u32, now: Instant) -> bool {
        let q = self.recent.entry(pane).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= per_minute as usize {
            return false;
        }
        q.push_back(now);
        true
    }

    pub(crate) fn forget(&mut self, pane: u64) {
        self.recent.remove(&pane);
    }
}

/// One notification ready for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Note {
    pub(crate) title: String,
    pub(crate) body: String,
    /// freedesktop urgency byte: 0 low, 1 normal, 2 critical.
    pub(crate) urgency: u8,
}

fn cut(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].chars().filter(|c| !c.is_control()).collect()
}

impl Note {
    pub(crate) fn new(title: &str, body: &str, urgency: u8) -> Note {
        Note {
            title: cut(title, TITLE_MAX),
            body: cut(body, BODY_MAX),
            urgency: urgency.min(2),
        }
    }

    /// An OSC notification: OSC 777's `title: body` text is split back; the others are a
    /// body under the pane title.
    pub(crate) fn from_osc(source: u16, text: &str, pane_title: &str) -> Note {
        if source == 777
            && let Some((t, b)) = text.split_once(": ")
        {
            return Note::new(t, b, 1);
        }
        let title = if pane_title.is_empty() {
            "Apotheosis"
        } else {
            pane_title
        };
        Note::new(title, text, 1)
    }
}

/// Send `note` on the session bus (blocking; run it on a worker). Failures are counted in
/// the log by kind only.
pub(crate) fn deliver(app_name: &str, note: &Note) {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(_) => {
            tracing::debug!("no session bus; notification dropped");
            return;
        }
    };
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("urgency", Value::U8(note.urgency));
    let r = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            app_name,
            0u32,
            "utilities-terminal",
            note.title.as_str(),
            note.body.as_str(),
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    );
    if r.is_err() {
        tracing::debug!("notification service refused the call");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_filters() {
        assert!(when_allows(When::Unfocused, false, true));
        assert!(!when_allows(When::Unfocused, true, true));
        assert!(when_allows(When::Invisible, true, false));
        assert!(!when_allows(When::Invisible, false, true));
        assert!(when_allows(When::Always, true, true));
        assert!(!when_allows(When::Never, false, false));
    }

    #[test]
    fn rate_gate_is_per_pane_per_minute() {
        let mut g = RateGate::default();
        let t = Instant::now();
        assert_eq!((0..5).filter(|_| g.admit(1, 3, t)).count(), 3);
        assert!(g.admit(2, 3, t), "another pane has its own budget");
        assert!(!g.admit(1, 3, t + Duration::from_secs(59)));
        assert!(g.admit(1, 3, t + Duration::from_secs(60)));
    }

    #[test]
    fn sources_and_settings() {
        let mut s = NotifySettings::default();
        assert!(s.accepts(9) && s.accepts(99) && s.accepts(777));
        assert!(!s.accepts(4), "OSC 9;4 is progress, never a notification");
        s.osc99 = false;
        assert!(!s.accepts(99));
        let fs = apoth_config::fs::MemFs::new().with(
            "/c/a.kdl",
            "version 1\nnotifications { when \"always\"; osc-777 #false; max-per-minute 5; }\nbell { notify #true; }\n",
        );
        let r = apoth_config::load(
            &fs,
            &apoth_config::LoadInput {
                user: Some("/c/a.kdl".into()),
                ..apoth_config::LoadInput::default()
            },
            None,
        );
        let n = NotifySettings::from_config(&r.config);
        assert_eq!(n.when, When::Always);
        assert!(!n.osc777 && n.osc9);
        assert_eq!(n.per_minute, 5);
        assert!(n.bell);
    }

    #[test]
    fn notes_are_bounded_and_split() {
        let n = Note::from_osc(777, "build: done", "zsh");
        assert_eq!((n.title.as_str(), n.body.as_str()), ("build", "done"));
        let n = Note::from_osc(9, "hello", "");
        assert_eq!(n.title, "Apotheosis");
        let long = "é".repeat(BODY_MAX);
        let n = Note::new("t\x07", &long, 9);
        assert!(n.body.len() <= BODY_MAX);
        assert_eq!(n.title, "t");
        assert_eq!(n.urgency, 2);
    }
}
