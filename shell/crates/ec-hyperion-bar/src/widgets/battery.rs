// SPDX-License-Identifier: AGPL-3.0-only
//! Battery: the drawn gauge and its percentage. Important by default, so it
//! never compresses; present only on a machine that has one.
//!
//! A status widget: it reads `app.battery`, so it has no `State` of its own.
//! Clicking it opens the battery drawer (ADR 0065 amendment): the reading as a
//! magnitude hero, then the power profile selector driven by
//! `ec_services::power_profiles`.

use std::time::Duration;

use iced::widget::{button, container, text, Row};
use iced::{Alignment, Color, Length};

use ec_services::power_profiles::{Degraded, PowerState, Profile, Snapshot};
use ec_services::status::{Battery, Charge};
use ec_ui::tokens::{bar, color, drawer, font, size, space};
use ec_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{elide, Sheet, DRAWER_ERR_CHARS};

/// Percentage at or below which a discharging battery is drawn as a warning.
pub(crate) const LOW: u8 = 15;

/// What the power-profiles service last said, and what the drawer shows of it.
#[derive(Debug, Default)]
pub struct Power {
    /// `None` until the daemon has answered, or said it cannot.
    pub reading: Option<PowerState>,
    /// The profile the human last asked for, until the daemon agrees.
    asked: Option<Profile>,
    /// The daemon's reason a set failed. Shown until the human asks again.
    pub failed: Option<String>,
}

impl Power {
    /// Fold one event from the service into what the drawer shows.
    pub fn apply(&mut self, event: ec_services::power_profiles::Event) {
        use ec_services::power_profiles::Event;
        match event {
            Event::State(state) => {
                if let (Some(asked), PowerState::Ready(snap)) = (self.asked, &state) {
                    if snap.active == asked {
                        self.asked = None;
                        self.failed = None;
                    }
                }
                self.reading = Some(state);
            }
            // A refused set reverts the selector to what the daemon last said.
            Event::Failed { reason, .. } => {
                self.asked = None;
                self.failed = Some(reason);
            }
        }
    }

    /// The human picked a profile: the failure they were told about is stale.
    pub fn ask(&mut self, profile: Profile) {
        self.asked = Some(profile);
        self.failed = None;
    }

    /// The profile the selector shows as chosen: the one asked for until the
    /// daemon agrees, else the one it reports as active.
    pub fn shown(&self, active: Profile) -> Profile {
        self.asked.unwrap_or(active)
    }
}

/// Gauge, gap, then the text at its own length: a fixed cell clipped "76%"
/// against its edge, and "+100%" would not have fit at all.
pub fn spans(app: &App) -> Spans {
    let chars = app.battery.map_or(0, |b| battery_text(b).chars().count());
    Spans {
        core: space::MARK_CELL_W + bar::GAP + chars as f32 * bar::CHAR_W,
        revealed: 0.0,
        lead: 0.0,
        present: app.battery.is_some(),
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let Some(battery) = app.battery else {
        return Parts::empty();
    };
    let (gauge, reading) = gauge_and_reading(battery);
    let face = Row::new()
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .push(parts::battery_gauge_faded(
            battery.percent,
            gauge,
            ink,
            app.battery_flow(),
        ))
        .push(
            text(battery_text(battery))
                .size(size::MONO)
                .font(font::DATA)
                .color(reading.scale_alpha(ink)),
        );
    // The face is the press: the glass cell under it answers the pointer.
    let core = button(face)
        .padding(0)
        .style(super::bare)
        .on_press(Message::Open(Drawer::Battery));
    Parts {
        core: core.into(),
        revealed: None,
    }
}

/// Low and not charging is the one status the human has to act on, so it is
/// the one status allowed to leave the neutral palette. Full takes the
/// accent, on the gauge and the reading alike; charging stays neutral and lets
/// the energy flow carry the state.
fn gauge_and_reading(battery: Battery) -> (Color, Color) {
    if battery.percent <= LOW && battery.state == Charge::Discharging {
        (color::danger(), color::danger())
    } else if battery.state == Charge::Full {
        (color::accent(), color::accent_text())
    } else {
        (color::text_secondary(), color::text_secondary())
    }
}

/// Charging is a leading `+`; full and discharging are the bare percentage.
/// The drawer carries the time remaining; the cell does not, since it is the
/// least trustworthy number UPower reports.
pub(crate) fn battery_text(battery: Battery) -> String {
    match battery.state {
        Charge::Charging => format!("+{}%", battery.percent),
        Charge::Full | Charge::Discharging | Charge::Unknown => format!("{}%", battery.percent),
    }
}

/// The profile's name on the selector: the daemon's word, capitalised.
pub(crate) fn profile_label(profile: Profile) -> &'static str {
    match profile {
        Profile::PowerSaver => "Power saver",
        Profile::Balanced => "Balanced",
        Profile::Performance => "Performance",
    }
}

/// The charge's state, as the drawer's line under the reading.
fn state_word(state: Charge) -> &'static str {
    match state {
        Charge::Charging => "charging",
        Charge::Discharging => "on battery",
        Charge::Full => "fully charged",
        Charge::Unknown => "battery",
    }
}

/// Time remaining, when UPower has one: "2 h 05 m left" on battery,
/// "1 h 10 m to full" charging, nothing otherwise.
pub(crate) fn remaining_text(battery: Battery) -> String {
    let Some(left) = battery.remaining else {
        return String::new();
    };
    let span = clock(left);
    match battery.state {
        Charge::Discharging => format!("{span} left"),
        Charge::Charging => format!("{span} to full"),
        Charge::Full | Charge::Unknown => String::new(),
    }
}

/// `H h MM m`, or `M m` under an hour.
fn clock(d: Duration) -> String {
    let minutes = d.as_secs() / 60;
    if minutes >= 60 {
        format!("{} h {:02} m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} m")
    }
}

/// Why the selector reads the way it does, as one line under it: a
/// performance profile the machine does not offer, or one the daemon says is
/// limited right now. `None` when there is nothing to say.
pub(crate) fn note_for(snap: &Snapshot) -> Option<String> {
    let mut reasons = Vec::new();
    if !snap.profiles.contains(&Profile::Performance) {
        reasons.push("performance not offered".to_owned());
    }
    match &snap.degraded {
        Some(Degraded::LapDetected) => reasons.push("limited: lap detected".to_owned()),
        Some(Degraded::HighOperatingTemperature) => {
            reasons.push("limited: high temperature".to_owned());
        }
        Some(Degraded::Other(reason)) => reasons.push(format!("limited: {reason}")),
        None => {}
    }
    (!reasons.is_empty()).then(|| reasons.join(" \u{b7} "))
}

/// The battery drawer. See `view::drawer_view` for its anatomy and ledger.
///
/// Anatomy: the head; the magnitude hero (the percentage, its state and the
/// time left); the selector, one segmented row; one line of why, or the
/// daemon's refusal in danger ink; the way out to Session settings.
///
/// Ledger: the reading is the one live value. It takes the accent only when
/// the battery is full (the bar's own rule), danger when it is low and
/// draining, and the text ink otherwise. The selection is neutral.
pub(crate) fn sheet(app: &App) -> Sheet {
    let mut s = Sheet::new(parts::drawer_sheet_head("battery"), drawer::HEAD_H);
    let Some(battery) = app.battery else {
        s.row(parts::drawer_note("no battery"));
        return s;
    };
    let (_, tint) = gauge_and_reading(battery);
    s.block(
        parts::drawer_magnitude(
            &battery.percent.to_string(),
            "%",
            tint,
            state_word(battery.state),
            &remaining_text(battery),
        ),
        drawer::MAGNITUDE_H,
    );
    match &app.power.reading {
        Some(PowerState::Ready(snap)) => {
            let options = Profile::ALL
                .into_iter()
                .map(|profile| {
                    let offered = snap.profiles.contains(&profile);
                    // Performance is greyed while the machine does not offer it
                    // or the daemon says it is limited: the reason is the note.
                    let limited = profile == Profile::Performance && (!offered || snap.degraded.is_some());
                    let press = (offered && !limited).then_some(Message::PowerSet(profile));
                    (
                        profile_label(profile),
                        app.power.shown(snap.active) == profile,
                        press,
                    )
                })
                .collect();
            s.block(
                container(parts::segment_control(options))
                    .width(Length::Fill)
                    .height(Length::Fixed(drawer::ROW_H))
                    .align_y(Alignment::Center)
                    .padding([0.0, drawer::ROW_X])
                    .into(),
                drawer::ROW_H,
            );
        }
        Some(PowerState::Unavailable) => s.row(parts::drawer_note("profiles unavailable")),
        None => {}
    }
    if let Some(reason) = &app.power.failed {
        let line = format!("couldn't set: {reason}");
        s.row(parts::drawer_error(&elide(&line, DRAWER_ERR_CHARS)));
    } else if let Some(PowerState::Ready(snap)) = &app.power.reading {
        if let Some(note) = note_for(snap) {
            s.row(parts::drawer_note(&elide(&note, DRAWER_ERR_CHARS)));
        }
    }
    s.rule();
    s.row(parts::drawer_link(
        "Session settings",
        Message::OpenSettings("session"),
    ));
    s
}

/// The fixture's power reading, from `HYPERION_PREVIEW_POWER`: every state the
/// drawer draws, without a daemon to ask. Debug builds only.
#[cfg(debug_assertions)]
pub fn preview(power: &mut Power) {
    // Unset means the default fixture: balanced, every profile offered.
    let which = std::env::var("HYPERION_PREVIEW_POWER").unwrap_or_default();
    let all = Profile::ALL.to_vec();
    let ready = |active, profiles: Vec<Profile>, degraded| {
        PowerState::Ready(Snapshot {
            active,
            profiles,
            degraded,
        })
    };
    power.reading = Some(match which.as_str() {
        "unavailable" => PowerState::Unavailable,
        "saver" => ready(Profile::PowerSaver, all, None),
        "performance" => ready(Profile::Performance, all, None),
        "degraded" => ready(Profile::Performance, all, Some(Degraded::LapDetected)),
        "no-performance" => ready(
            Profile::Balanced,
            vec![Profile::PowerSaver, Profile::Balanced],
            None,
        ),
        "failed" => {
            power.failed = Some("power-profiles-daemon is busy".to_owned());
            ready(Profile::Balanced, all, None)
        }
        _ => ready(Profile::Balanced, all, None),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::power_profiles::Event;

    #[test]
    fn battery_text_marks_charging_and_shows_percent_when_full() {
        let b = |percent, state| Battery {
            percent,
            state,
            remaining: None,
        };
        assert_eq!(battery_text(b(40, Charge::Charging)), "+40%");
        assert_eq!(battery_text(b(100, Charge::Full)), "100%");
        assert_eq!(battery_text(b(7, Charge::Discharging)), "7%");
        assert_eq!(battery_text(b(15, Charge::Unknown)), "15%");
    }

    #[test]
    fn the_drawer_says_how_long_is_left_only_when_upower_knows() {
        let b = |state, remaining| Battery {
            percent: 60,
            state,
            remaining,
        };
        let two_and_a_bit = Some(Duration::from_secs(2 * 3600 + 5 * 60));
        assert_eq!(
            remaining_text(b(Charge::Discharging, two_and_a_bit)),
            "2 h 05 m left"
        );
        assert_eq!(
            remaining_text(b(Charge::Charging, Some(Duration::from_secs(70 * 60)))),
            "1 h 10 m to full"
        );
        assert_eq!(
            remaining_text(b(Charge::Discharging, Some(Duration::from_secs(9 * 60)))),
            "9 m left"
        );
        assert_eq!(remaining_text(b(Charge::Discharging, None)), "");
        assert_eq!(remaining_text(b(Charge::Full, two_and_a_bit)), "");
    }

    #[test]
    fn a_low_battery_on_battery_is_danger_and_full_is_the_accent() {
        let b = |percent, state| Battery {
            percent,
            state,
            remaining: None,
        };
        assert_eq!(gauge_and_reading(b(LOW, Charge::Discharging)).1, color::danger());
        assert_eq!(gauge_and_reading(b(100, Charge::Full)).1, color::accent_text());
        assert_eq!(
            gauge_and_reading(b(LOW, Charge::Charging)).1,
            color::text_secondary()
        );
    }

    #[test]
    fn the_selector_note_names_what_limits_performance() {
        let snap = |profiles: Vec<Profile>, degraded| Snapshot {
            active: Profile::Balanced,
            profiles,
            degraded,
        };
        let all = Profile::ALL.to_vec();
        assert_eq!(note_for(&snap(all.clone(), None)), None);
        assert_eq!(
            note_for(&snap(all.clone(), Some(Degraded::LapDetected))).as_deref(),
            Some("limited: lap detected")
        );
        assert_eq!(
            note_for(&snap(vec![Profile::PowerSaver, Profile::Balanced], None)).as_deref(),
            Some("performance not offered")
        );
    }

    #[test]
    fn a_failed_set_is_shown_until_the_human_asks_again() {
        let mut power = Power::default();
        let ready = |active| {
            Event::State(PowerState::Ready(Snapshot {
                active,
                profiles: Profile::ALL.to_vec(),
                degraded: None,
            }))
        };
        power.apply(ready(Profile::Balanced));
        power.ask(Profile::Performance);
        power.apply(Event::Failed {
            action: "set_profile",
            reason: "refused".to_owned(),
        });
        // The daemon's follow-up reading still says Balanced: the note stays.
        power.apply(ready(Profile::Balanced));
        assert_eq!(power.failed.as_deref(), Some("refused"));
        // The daemon agrees with what was asked: the note goes.
        power.ask(Profile::Performance);
        power.apply(ready(Profile::Performance));
        assert_eq!(power.failed, None);
    }

    #[test]
    fn a_pressed_profile_is_shown_as_chosen_until_the_daemon_agrees() {
        let mut power = Power::default();
        let ready = |active| {
            Event::State(PowerState::Ready(Snapshot {
                active,
                profiles: Profile::ALL.to_vec(),
                degraded: None,
            }))
        };
        power.apply(ready(Profile::Balanced));
        power.ask(Profile::Performance);
        assert_eq!(power.shown(Profile::Balanced), Profile::Performance);
        power.apply(ready(Profile::Balanced));
        assert_eq!(
            power.shown(Profile::Balanced),
            Profile::Performance,
            "a reading that has not agreed yet keeps the choice shown"
        );
        power.apply(Event::Failed {
            action: "set_profile",
            reason: "refused".to_owned(),
        });
        assert_eq!(
            power.shown(Profile::Balanced),
            Profile::Balanced,
            "a refusal reverts the selector"
        );
        power.ask(Profile::Performance);
        power.apply(ready(Profile::Performance));
        assert_eq!(power.shown(Profile::Performance), Profile::Performance);
        assert_eq!(power.asked, None);
    }

    #[test]
    fn clock_reads_hours_and_minutes() {
        assert_eq!(clock(Duration::from_secs(59 * 60)), "59 m");
        assert_eq!(clock(Duration::from_secs(60 * 60)), "1 h 00 m");
    }
}
