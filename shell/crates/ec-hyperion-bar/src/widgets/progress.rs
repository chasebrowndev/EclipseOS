// SPDX-License-Identifier: AGPL-3.0-only
//! Progress: a cell that exists only while something is in flight (ADR 0065
//! amendment) — a compile, a download, a copy. With no job it is zero width
//! and draws nothing; it animates in and out through the shell's own presence
//! and width, like every other widget.
//!
//! A status widget, like battery: it reads `app.progress`, the list the
//! progress service last sent (oldest first), so it has no `State` of its own.
//! The cell shows the oldest job: a ring, its label, `N%` when the amount of
//! work is known, and `+N` when there are more. Clicking it opens the progress
//! drawer, which lists every job.
//!
//! Ledger: none. The ring and the bar are the neutral ink. The strip already
//! carries the one live yellow (the focused chip's label, a full battery), and
//! a second one here would compete with it. In-flight work is state, but the
//! strip has no room for a second yellow.

use std::time::Instant;

use ec_services::progress::Job;
use iced::widget::{button, row, text, Column, Row, Space};
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

/// The cell's text, decided once so its width and its drawing agree.
#[derive(Debug, Clone, PartialEq)]
struct Face {
    label: String,
    percent: Option<String>,
    more: Option<String>,
}

impl Face {
    fn of(jobs: &[Job]) -> Option<Face> {
        let job = jobs.first()?;
        let more = jobs.len() - 1;
        Some(Face {
            label: elide(&job.label, label_chars()),
            percent: job.fraction.map(percent_text),
            more: (more > 0).then(|| format!("+{more}")),
        })
    }

    /// The core's width: ring, label, then each of the mono readings after a gap.
    fn width(&self) -> f32 {
        let chars = |s: &str| s.chars().count() as f32 * bar::CHAR_W;
        let mut w = bar::PROGRESS_RING + bar::GAP + chars(&self.label);
        for part in [&self.percent, &self.more].into_iter().flatten() {
            w += bar::GAP + chars(part);
        }
        w
    }
}

/// A fraction as the cell and the drawer write it: `40%`, rounded.
fn percent_text(fraction: f32) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// Time since the job started: `m:ss`, or `h:mm:ss` past an hour.
fn elapsed(started: Instant) -> String {
    let secs = started.elapsed().as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
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
    let ring = parts::progress_ring(
        job.fraction,
        app.progress_phase(),
        bar::PROGRESS_RING,
        color::text().scale_alpha(ink),
    );
    let mut line = Row::new()
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .push(ring)
        .push(
            text(face.label)
                .font(font::UI)
                .size(size::BODY_SMALL)
                .color(color::text_secondary().scale_alpha(ink)),
        );
    if let Some(percent) = face.percent {
        line = line.push(
            text(percent)
                .font(font::DATA)
                .size(size::MONO)
                .color(color::text().scale_alpha(ink)),
        );
    }
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
/// Anatomy: the head; a magnitude hero for the job the cell shows (its percent,
/// or its elapsed time when the amount of work is unknown); a rule; then one
/// block per job, oldest first, up to [`drawer::MAX_ROWS`], and a note for the
/// rest.
///
/// Ledger: none. Every reading is the text ink.
pub(crate) fn sheet(app: &App) -> Sheet {
    let mut s = Sheet::new(parts::drawer_sheet_head("progress"), drawer::HEAD_H);
    let Some(lead) = app.progress.first() else {
        s.row(parts::drawer_note("nothing in progress"));
        return s;
    };
    let (number, unit) = match lead.fraction {
        Some(f) => {
            let p = percent_text(f);
            (p.trim_end_matches('%').to_owned(), "%")
        }
        None => (elapsed(lead.started), ""),
    };
    let detail = format!("{} \u{b7} {}", lead.source.as_str(), elapsed(lead.started));
    s.block(
        parts::drawer_magnitude(
            &number,
            unit,
            color::text(),
            &elide(&lead.label, drawer::JOB_LABEL_CHARS),
            &detail,
        ),
        drawer::MAGNITUDE_H,
    );
    s.rule();
    let phase = app.progress_phase();
    for job in app.progress.iter().take(drawer::MAX_ROWS) {
        s.block(job_row(job, phase), drawer::JOB_H);
    }
    let rest = app.progress.len().saturating_sub(drawer::MAX_ROWS);
    if rest > 0 {
        s.row(parts::drawer_note(&format!("+{rest} more")));
    }
    s
}

/// One job's block: a ring and its label with the reading at the right, its
/// bar when the amount of work is known (a spacer of the same height when not,
/// so every job is the same silhouette), and a mono line for the source and
/// the time it has run.
fn job_row(job: &Job, phase: f32) -> Element<'static, Message, Theme> {
    let reading = job.fraction.map(percent_text).unwrap_or_default();
    let head = row![
        parts::progress_ring(job.fraction, phase, drawer::MARK, color::text()),
        text(elide(&job.label, drawer::JOB_LABEL_CHARS))
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL)
            .color(color::text())
            .width(Length::Fill),
        text(reading)
            .font(font::DATA)
            .size(size::MONO)
            .color(color::text_secondary()),
    ]
    .spacing(drawer::MARK_GAP)
    .align_y(Alignment::Center);
    let bar: Element<'static, Message, Theme> = match job.fraction {
        Some(f) => parts::progress_bar(f, color::text()),
        None => Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(ec_ui::tokens::console::BAR_H))
            .into(),
    };
    let meta = text(format!("{} \u{b7} {}", job.source.as_str(), elapsed(job.started)))
        .font(font::DATA)
        .size(size::MICRO)
        .color(color::text_tertiary());
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
    use std::time::Duration;

    fn job(id: &str, label: &str, fraction: Option<f32>) -> Job {
        Job {
            id: id.to_owned(),
            label: label.to_owned(),
            source: Source::Feed,
            fraction,
            started: Instant::now(),
        }
    }

    #[test]
    fn no_job_means_no_face_and_no_width() {
        assert_eq!(Face::of(&[]), None);
    }

    #[test]
    fn one_determinate_job_shows_its_label_and_percent() {
        let jobs = [job("a", "Demo copy", Some(0.4))];
        let face = Face::of(&jobs).expect("a face");
        assert_eq!(face.label, "Demo copy");
        assert_eq!(face.percent.as_deref(), Some("40%"));
        assert_eq!(face.more, None);
    }

    #[test]
    fn an_indeterminate_job_shows_no_percent() {
        let jobs = [job("a", "make", None)];
        let face = Face::of(&jobs).expect("a face");
        assert_eq!(face.percent, None);
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
    fn the_width_grows_with_each_reading() {
        let one = Face {
            label: "copy".to_owned(),
            percent: None,
            more: None,
        };
        let with_pct = Face {
            percent: Some("40%".to_owned()),
            ..one.clone()
        };
        assert!(with_pct.width() > one.width());
        assert!(
            Face {
                more: Some("+1".to_owned()),
                ..with_pct.clone()
            }
            .width()
                > with_pct.width()
        );
    }

    #[test]
    fn percent_rounds_and_clamps() {
        assert_eq!(percent_text(0.404), "40%");
        assert_eq!(percent_text(0.996), "100%");
        assert_eq!(percent_text(-0.2), "0%");
        assert_eq!(percent_text(3.0), "100%");
    }

    #[test]
    fn elapsed_reads_minutes_then_hours() {
        let ago = |secs: u64| Instant::now() - Duration::from_secs(secs);
        assert_eq!(elapsed(ago(72)), "1:12");
        assert_eq!(elapsed(ago(5)), "0:05");
        assert_eq!(elapsed(ago(3 * 3600 + 61)), "3:01:01");
    }
}
