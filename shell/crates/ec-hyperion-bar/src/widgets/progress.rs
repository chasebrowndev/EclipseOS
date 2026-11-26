// SPDX-License-Identifier: AGPL-3.0-only
//! Progress: a cell that exists only while something is in flight (ADR 0065
//! amendment) — a compile, a download, a copy. With no job it is zero width
//! and draws nothing; it animates in and out through the shell's own presence
//! and width, like every other widget.
//!
//! A status widget, like battery: it reads `app.progress`, the list the
//! progress service last sent (oldest first), so it has no `State` of its own.
//! The cell shows the oldest job as a gauge: a rounded-square ring (the
//! battery glance's) that fills clockwise round its perimeter from the top
//! centre to the job's fraction, or, when the amount of work is unknown and
//! there is no estimate, a short soft sweep travelling round the same ring,
//! which is not a percent. The label (cut to a width) and `+N` when
//! there are more sit beside it. The cell carries no percent and no ETA text:
//! the gauge is the reading. Clicking it opens the progress drawer, which lists
//! every job with its exact percent, bar and ETA.
//!
//! Estimates never pose as exact. A job's ETA is written plainly when it was
//! reported or measured (`2m 10s left`) and with a leading `~` when it is only
//! the median of past runs (`~2m 10s left`). A job with no real fraction but a
//! history ETA fills from an estimate, elapsed / (elapsed + remaining), held
//! under 95% and never drawn backwards, the same fill as a real fraction; the
//! drawer writes it `~40%`. A real fraction always wins; with neither, the
//! sweep.
//!
//! Ledger: none. The gauge and the bar are the neutral ink. The strip already
//! carries the one live yellow (the focused chip's label, a full battery), and
//! a second one here would compete with it. In-flight work is state, but the
//! strip has no room for a second yellow.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ec_services::progress::{Eta, EtaBasis, Job};
use iced::widget::{button, text, text::Wrapping, Column, Row, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_ui::tokens::{bar, color, drawer, font, size};
use ec_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{elide, Sheet};

/// The label's width in characters, from the width the cell gives it.
fn label_chars() -> usize {
    (bar::PROGRESS_LABEL_W / bar::CHAR_W) as usize
}

/// The cell's text, decided once so its width and its drawing agree. The gauge
/// is drawn from the job itself; only the words are here.
#[derive(Debug, Clone, PartialEq)]
struct Face {
    label: String,
    more: Option<String>,
}

impl Face {
    fn of(jobs: &[Job]) -> Option<Face> {
        let job = jobs.first()?;
        let more = jobs.len() - 1;
        Some(Face {
            label: elide(&job.label, label_chars()),
            more: (more > 0).then(|| format!("+{more}")),
        })
    }

    /// The core's width: the gauge, the label, then `+N` after a gap.
    fn width(&self) -> f32 {
        let chars = |s: &str| s.chars().count() as f32 * bar::CHAR_W;
        let mut w = bar::PROGRESS_GAUGE + bar::GAP + chars(&self.label);
        if let Some(more) = &self.more {
            w += bar::GAP + chars(more);
        }
        w
    }
}

/// An estimate never reads as done.
const ESTIMATE_CAP: f32 = 0.95;

/// How far a job is: measured by the source, estimated from history, or not
/// known at all.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Reading {
    Exact(f32),
    Estimate(f32),
    Unknown,
}

impl Reading {
    /// The fill to draw, for the same gauge and bar either way.
    fn fill(self) -> Option<f32> {
        match self {
            Reading::Exact(f) | Reading::Estimate(f) => Some(f),
            Reading::Unknown => None,
        }
    }
}

/// An estimated fill for a job with no real fraction but a history ETA:
/// `elapsed / (elapsed + remaining)`, clamped to `[0, 0.95]`. `None` for any
/// other job, so a real fraction (or a reported or measured ETA) is never
/// replaced.
fn estimated_fraction(job: &Job, now: Instant) -> Option<f32> {
    if job.fraction.is_some() {
        return None;
    }
    let eta = job.eta.as_ref().filter(|e| e.basis == EtaBasis::History)?;
    let elapsed = now.saturating_duration_since(job.started).as_secs_f32();
    let total = elapsed + eta.remaining.as_secs_f32();
    let f = if total > 0.0 { elapsed / total } else { 0.0 };
    Some(f.clamp(0.0, ESTIMATE_CAP))
}

/// Raise each estimated job's high-water mark to `now`'s estimate and drop the
/// marks of jobs that are gone. Called on every service update and frame.
pub(crate) fn track(jobs: &[Job], marks: &mut HashMap<String, f32>, now: Instant) {
    marks.retain(|id, _| jobs.iter().any(|j| j.id == *id));
    for job in jobs {
        if let Some(f) = estimated_fraction(job, now) {
            let m = marks.entry(job.id.clone()).or_insert(0.0);
            *m = m.max(f);
        } else {
            marks.remove(&job.id);
        }
    }
}

/// The reading to draw for a job now. The estimate is taken at the frame's
/// own time and held at the job's mark, so it creeps between updates and
/// never steps back.
fn reading(job: &Job, marks: &HashMap<String, f32>, now: Instant) -> Reading {
    if let Some(f) = job.fraction {
        return Reading::Exact(f);
    }
    match estimated_fraction(job, now) {
        Some(f) => Reading::Estimate(f.max(marks.get(&job.id).copied().unwrap_or(0.0))),
        None => Reading::Unknown,
    }
}

/// A fraction as the drawer writes it: `40%`, rounded.
fn percent_text(fraction: f32) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// A span of time, compactly: `45s`, `2m 10s`, `1h 05m`. Past an hour the
/// seconds are noise and go.
fn compact(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, secs / 60 % 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// A reading as the drawer writes it: `40%`, or `~40%` for an estimate, and
/// nothing when unknown.
fn reading_text(r: Reading) -> String {
    match r {
        Reading::Exact(f) => percent_text(f),
        Reading::Estimate(f) => format!("~{}", percent_text(f.min(ESTIMATE_CAP))),
        Reading::Unknown => String::new(),
    }
}

/// An estimate is only approximate when it comes from past runs.
fn approximate(eta: &Eta) -> bool {
    eta.basis == EtaBasis::History
}

/// An ETA as the drawer writes it: `2m 10s left`, with a leading `~` when the
/// estimate is approximate (`~2m 10s left`).
fn eta_text(eta: &Eta) -> String {
    let mark = if approximate(eta) { "~" } else { "" };
    format!("{mark}{} left", compact(eta.remaining))
}

pub fn spans(app: &App) -> Spans {
    match Face::of(&app.progress) {
        Some(face) => Spans {
            core: face.width(),
            revealed: 0.0,
            lead: 0.0,
            present: true,
        },
        None => Spans::default(),
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let Some(face) = Face::of(&app.progress) else {
        return Parts::empty();
    };
    let ink = frame.core_alpha();
    let job = &app.progress[0];
    let gauge = parts::progress_squircle(
        reading(job, &app.progress_marks, Instant::now()).fill(),
        app.progress_phase(),
        bar::PROGRESS_GAUGE,
        bar::PROGRESS_STROKE,
        color::text().scale_alpha(ink),
    );
    let mut line = Row::new()
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .push(gauge)
        .push(
            text(face.label)
                .font(font::UI)
                .size(size::BODY_SMALL)
                .wrapping(Wrapping::None)
                .color(color::text_secondary().scale_alpha(ink)),
        );
    if let Some(more) = face.more {
        line = line.push(
            text(more)
                .font(font::DATA)
                .size(size::MONO)
                .color(color::text_tertiary().scale_alpha(ink)),
        );
    }
    // The face is the press: the glass cell under it answers the pointer.
    let core = button(line)
        .padding(iced::Padding::ZERO)
        .style(super::bare)
        .on_press(Message::Open(Drawer::Progress));
    Parts {
        core: core.into(),
        revealed: None,
    }
}

/// The progress drawer. See `view::drawer_view` for its anatomy and ledger.
///
/// Anatomy: no head and no hero; the drawer opens straight onto one block per
/// job, oldest first, up to [`drawer::MAX_ROWS`], and a note for the rest. A
/// job is its label with the exact percent at the right, its bar, and its ETA
/// as one quiet line under the bar. Every block is the same height, so an
/// ETA arriving never moves the list.
///
/// Ledger: none. Every reading is the text ink.
pub(crate) fn sheet(app: &App) -> Sheet {
    // The clock drawer's headless sheet: the frame stays, the title goes.
    let mut s = Sheet::new(Space::new().into(), 0.0);
    if app.progress.is_empty() {
        s.row(parts::drawer_note("nothing in progress"));
        return s;
    }
    let phase = app.progress_phase();
    let now = Instant::now();
    for job in app.progress.iter().take(drawer::MAX_ROWS) {
        let r = reading(job, &app.progress_marks, now);
        s.block(job_row(job, r, phase), drawer::JOB_H);
    }
    let rest = app.progress.len().saturating_sub(drawer::MAX_ROWS);
    if rest > 0 {
        s.row(parts::drawer_note(&format!("+{rest} more")));
    }
    s
}

/// One job's block: its label with the percent at the right (`~` when
/// estimated, nothing when the amount is unknown), its bar (a soft sweep along the track when it is
/// unknown, so an unknown job never looks like an amount), and the ETA as a
/// quiet mono line right-aligned under the bar. The line is always laid out,
/// empty when there is no ETA, so every job is the same height.
fn job_row(job: &Job, reading: Reading, phase: f32) -> Element<'static, Message, Theme> {
    let reading_label = reading_text(reading);
    let head = Row::new()
        .spacing(drawer::MARK_GAP)
        .align_y(Alignment::Center)
        .push(
            text(elide(&job.label, drawer::JOB_LABEL_CHARS))
                .font(font::UI_MEDIUM)
                .size(size::BODY_SMALL)
                .wrapping(Wrapping::None)
                .color(color::text())
                .width(Length::Fill),
        )
        .push(
            text(reading_label)
                .font(font::DATA)
                .size(size::MONO)
                .color(color::text()),
        );
    let bar: Element<'static, Message, Theme> = match reading.fill() {
        Some(f) => parts::progress_bar(f, color::text()),
        None => parts::progress_sweep_bar(phase, color::text()),
    };
    let meta = Row::new().push(Space::new().width(Length::Fill)).push(
        text(job.eta.as_ref().map(eta_text).unwrap_or_default())
            .font(font::DATA)
            .size(size::MICRO)
            .color(color::text_secondary()),
    );
    Column::new()
        .spacing(drawer::LINE_GAP)
        .padding([drawer::JOB_PAD, drawer::ROW_X])
        .width(Length::Fill)
        .push(head)
        .push(bar)
        .push(meta)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::progress::Source;
    use std::time::Instant;

    fn job(id: &str, label: &str, fraction: Option<f32>) -> Job {
        Job {
            id: id.to_owned(),
            label: label.to_owned(),
            source: Source::Feed,
            fraction,
            started: Instant::now(),
            eta: None,
        }
    }

    fn eta(secs: u64, basis: EtaBasis) -> Eta {
        Eta {
            remaining: Duration::from_secs(secs),
            basis,
        }
    }

    #[test]
    fn no_job_means_no_face_and_no_width() {
        assert_eq!(Face::of(&[]), None);
    }

    #[test]
    fn one_job_shows_its_label_and_no_count() {
        let jobs = [job("a", "Demo copy", Some(0.4))];
        let face = Face::of(&jobs).expect("a face");
        assert_eq!(face.label, "Demo copy");
        assert_eq!(face.more, None);
    }

    #[test]
    fn several_jobs_show_the_oldest_and_a_count_of_the_rest() {
        let jobs = [
            job("a", "pacman -Syu", None),
            job("b", "rsync", Some(0.9)),
            job("c", "curl", Some(0.1)),
        ];
        let face = Face::of(&jobs).expect("a face");
        assert_eq!(face.label, "pacman -Syu");
        assert_eq!(face.more.as_deref(), Some("+2"));
    }

    #[test]
    fn a_long_label_is_cut_to_the_label_width() {
        let jobs = [job("a", &"x".repeat(60), Some(0.5))];
        let label = Face::of(&jobs).expect("a face").label;
        assert_eq!(label.chars().count(), label_chars());
        assert!(label.ends_with('\u{2026}'));
    }

    #[test]
    fn the_width_is_the_gauge_the_label_and_the_count() {
        let one = Face {
            label: "copy".to_owned(),
            more: None,
        };
        assert_eq!(one.width(), bar::PROGRESS_GAUGE + bar::GAP + 4.0 * bar::CHAR_W);
        let more = Face {
            more: Some("+1".to_owned()),
            ..one.clone()
        };
        assert_eq!(more.width(), one.width() + bar::GAP + 2.0 * bar::CHAR_W);
    }

    #[test]
    fn the_width_does_not_depend_on_the_reading() {
        // The gauge carries the reading, so a percent or an ETA arriving can
        // never resize the cell.
        let mut a = job("a", "copy", None);
        let before = Face::of(std::slice::from_ref(&a)).expect("a face").width();
        a.fraction = Some(0.5);
        a.eta = Some(eta(130, EtaBasis::Rate));
        assert_eq!(Face::of(&[a]).expect("a face").width(), before);
    }

    #[test]
    fn percent_rounds_and_clamps() {
        assert_eq!(percent_text(0.404), "40%");
        assert_eq!(percent_text(0.996), "100%");
        assert_eq!(percent_text(-0.2), "0%");
        assert_eq!(percent_text(3.0), "100%");
    }

    #[test]
    fn spans_read_seconds_then_minutes_then_hours() {
        let s = Duration::from_secs;
        assert_eq!(compact(s(1)), "1s");
        assert_eq!(compact(s(45)), "45s");
        assert_eq!(compact(s(60)), "1m 00s");
        assert_eq!(compact(s(130)), "2m 10s");
        assert_eq!(compact(s(3599)), "59m 59s");
        assert_eq!(compact(s(3900)), "1h 05m");
        assert_eq!(compact(s(2 * 3600 + 59)), "2h 00m");
    }

    #[test]
    fn a_measured_eta_is_written_plainly() {
        assert_eq!(eta_text(&eta(130, EtaBasis::Rate)), "2m 10s left");
        assert_eq!(eta_text(&eta(45, EtaBasis::Reported)), "45s left");
        assert!(!approximate(&eta(45, EtaBasis::Reported)));
        assert!(!approximate(&eta(45, EtaBasis::Rate)));
    }

    fn at(j: &Job, secs: u64) -> Instant {
        j.started + Duration::from_secs(secs)
    }

    #[test]
    fn an_estimate_needs_no_fraction_and_a_history_eta() {
        let mut j = job("a", "cargo", None);
        assert_eq!(estimated_fraction(&j, at(&j, 60)), None);
        j.eta = Some(eta(60, EtaBasis::Rate));
        assert_eq!(estimated_fraction(&j, at(&j, 60)), None);
        j.eta = Some(eta(60, EtaBasis::History));
        assert_eq!(estimated_fraction(&j, at(&j, 60)), Some(0.5));
        j.fraction = Some(0.9);
        assert_eq!(estimated_fraction(&j, at(&j, 60)), None);
    }

    #[test]
    fn an_estimate_is_held_under_done() {
        let mut j = job("a", "cargo", None);
        j.eta = Some(eta(0, EtaBasis::History));
        assert_eq!(estimated_fraction(&j, at(&j, 600)), Some(ESTIMATE_CAP));
    }

    #[test]
    fn a_real_fraction_wins_and_is_not_altered() {
        let mut j = job("a", "cp", Some(0.4));
        j.eta = Some(eta(60, EtaBasis::History));
        let marks = HashMap::from([("a".to_owned(), 0.9)]);
        assert_eq!(reading(&j, &marks, at(&j, 60)), Reading::Exact(0.4));
    }

    #[test]
    fn no_estimate_means_the_sweep() {
        let j = job("a", "cargo", None);
        assert_eq!(reading(&j, &HashMap::new(), at(&j, 5)).fill(), None);
    }

    #[test]
    fn the_mark_never_goes_backwards_and_resets_with_the_job() {
        let mut j = job("a", "cargo", None);
        j.eta = Some(eta(60, EtaBasis::History));
        let mut marks = HashMap::new();
        track(std::slice::from_ref(&j), &mut marks, at(&j, 60));
        assert_eq!(marks["a"], 0.5);
        // A fresh update with a longer ETA would estimate lower.
        j.eta = Some(eta(600, EtaBasis::History));
        track(std::slice::from_ref(&j), &mut marks, at(&j, 60));
        assert_eq!(marks["a"], 0.5);
        assert_eq!(reading(&j, &marks, at(&j, 60)), Reading::Estimate(0.5));
        track(&[], &mut marks, at(&j, 61));
        assert!(marks.is_empty());
    }

    #[test]
    fn an_estimate_is_written_with_a_tilde() {
        assert_eq!(reading_text(Reading::Exact(0.4)), "40%");
        assert_eq!(reading_text(Reading::Estimate(0.4)), "~40%");
        assert_eq!(reading_text(Reading::Estimate(1.0)), "~95%");
        assert_eq!(reading_text(Reading::Unknown), "");
    }

    #[test]
    fn a_history_eta_is_marked_approximate() {
        let e = eta(130, EtaBasis::History);
        assert!(approximate(&e));
        assert_eq!(eta_text(&e), "~2m 10s left");
    }
}
