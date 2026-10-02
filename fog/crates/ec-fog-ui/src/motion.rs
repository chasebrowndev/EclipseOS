// SPDX-License-Identifier: AGPL-3.0-only

//! Motion (FOG §Visual design): springs for the sidebar, the tray, the
//! sheets, the selection pill and the hover wash, and the blurred snapshot
//! a sheet floats on.
//!
//! Nothing here gates input. State changes at once; the springs only follow
//! it on screen, stepped by `window::frames` while any of them moves. Under
//! `reduce-motion`, or with the compositor's animations off, every spring
//! snaps.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ec_fog_widgets::{Backdrop, Params, Spring};
use iced::window::{self, Screenshot};
use iced::Task;

use crate::app::{App, Message, Overlay};
use crate::ops::{NameEntry, NameFor};
use crate::theme::{look, motion};

/// A cursor jump longer than this many rows snaps instead of sweeping.
const PILL_JUMP: f32 = 8.0;
/// How long a sheet waits for its snapshot before opening without blur.
const SNAPSHOT_WAIT: Duration = Duration::from_millis(160);

#[derive(Debug)]
pub struct Motion {
    /// 0 closed to 1 open, for every sheet: palette, dialogs, name sheet.
    pub sheet: Spring,
    sheet_open: bool,
    /// The window as it was when the sheet opened, blurred under it.
    pub backdrop: Option<Arc<Backdrop>>,
    /// A snapshot was asked for at this instant and has not come.
    waiting: Option<Instant>,
    pub sidebar: Spring,
    pub tray: Spring,
    /// The list cursor's row, fractional while the pill slides.
    pub pill: Spring,
    /// The tab and folder the pill is in: a new one places it at once.
    pill_in: Option<(usize, Vec<u8>)>,
    /// The entry under the pill: when the listing shifts beneath an
    /// unmoved cursor, the pill is placed, not slid.
    pill_on: Option<Vec<u8>>,
    /// The row under the pointer and its wash, 0 to 1.
    pub hovered: Option<usize>,
    pub hover: Spring,
    last: Option<Instant>,
}

impl Motion {
    pub fn new(sidebar: bool) -> Motion {
        Motion {
            sheet: Spring::at(0.0),
            sheet_open: false,
            backdrop: None,
            waiting: None,
            sidebar: Spring::at(if sidebar { 1.0 } else { 0.0 }),
            tray: Spring::at(0.0),
            pill: Spring::at(0.0),
            pill_in: None,
            pill_on: None,
            hovered: None,
            hover: Spring::at(0.0),
            last: None,
        }
    }

    /// Whether frames are needed.
    pub fn busy(&self) -> bool {
        self.waiting.is_some()
            || [self.sheet, self.sidebar, self.tray, self.pill, self.hover]
                .iter()
                .any(|s| !s.settled())
    }

    /// One frame at `now`.
    pub fn frame(&mut self, now: Instant) {
        let dt = self
            .last
            .map(|l| now.saturating_duration_since(l))
            .unwrap_or(Duration::from_micros(16_667));
        if self
            .waiting
            .is_some_and(|w| now.duration_since(w) >= SNAPSHOT_WAIT)
        {
            // No snapshot in time: open on the plain tint.
            self.waiting = None;
            go(&mut self.sheet, 1.0, bouncy(motion::SHEET));
        }
        let l = look();
        for (s, p) in [
            (&mut self.sheet, l.bouncy(motion::SHEET)),
            (&mut self.sidebar, l.spring(motion::PANEL)),
            (&mut self.tray, l.spring(motion::PANEL)),
            (&mut self.pill, l.spring(motion::PILL)),
            (&mut self.hover, l.spring(motion::HOVER)),
        ] {
            match p {
                Some(p) => s.step(dt, p),
                None => s.snap(s.target),
            }
        }
        self.last = self.busy().then_some(now);
    }

    /// The snapshot for the open sheet arrived.
    pub fn backdrop(&mut self, shot: Screenshot) {
        if !self.sheet_open {
            return;
        }
        if let Some(blur) = look().blur {
            self.backdrop = Some(Backdrop::new(shot, blur));
        }
        if self.waiting.take().is_some() {
            go(&mut self.sheet, 1.0, bouncy(motion::SHEET));
        }
    }

    /// The look was rebuilt (fog.kdl reloaded). Without blur now, a sheet
    /// drops its blurred snapshot, or stops waiting for one; springs that
    /// can no longer move snap on the next frame.
    pub fn restyled(&mut self) {
        if look().blur.is_none() {
            self.backdrop = None;
            if self.waiting.take().is_some() {
                go(&mut self.sheet, 1.0, bouncy(motion::SHEET));
            }
        }
    }

    /// The window changed size: a snapshot no longer lines up.
    pub fn resized(&mut self) {
        self.backdrop = None;
    }

    pub fn hover(&mut self, row: Option<usize>) {
        if row == self.hovered {
            return;
        }
        self.hovered = row;
        if row.is_some() {
            self.hover.snap(0.0);
            go(&mut self.hover, 1.0, look().spring(motion::HOVER));
        }
    }
}

fn bouncy(settle: Duration) -> Option<Params> {
    look().bouncy(settle)
}

/// Retarget `s`; snap when there is no motion.
fn go(s: &mut Spring, target: f32, p: Option<Params>) {
    match p {
        Some(_) => s.to(target),
        None => s.snap(target),
    }
}

/// A sheet is up: something modal floats over the window.
pub fn sheet_open(app: &App) -> bool {
    !app.conflicts.is_empty()
        || matches!(
            app.overlay,
            Overlay::Palette(_)
                | Overlay::Confirm(_)
                | Overlay::Name(NameEntry {
                    what: NameFor::Folder | NameFor::File,
                    ..
                })
        )
}

/// Point the springs at the state `app` is now in. A sheet that just opened
/// asks for the window's snapshot to blur under it.
pub fn sync(app: &mut App) -> Task<Message> {
    let open = sheet_open(app);
    let sidebar = app.sidebar;
    let tray = !app.tray.jobs.is_empty();
    let b = app.tabs.active();
    let here = (app.tabs.index(), b.path.clone());
    let cursor = b.selected as f32;
    let on = b.row(b.selected).map(|e| e.name.clone());
    let rows = b.len();
    let l = look();
    let m = &mut app.motion;

    go(
        &mut m.sidebar,
        if sidebar { 1.0 } else { 0.0 },
        l.spring(motion::PANEL),
    );
    go(
        &mut m.tray,
        if tray { 1.0 } else { 0.0 },
        l.spring(motion::PANEL),
    );
    // The row under the pointer is the pointer's: a new folder or tab, or
    // a listing that shrank past it, leaves no wash on a row it never
    // entered (its own `Unhover` never comes once the row is gone).
    if m.pill_in.as_ref() != Some(&here) || m.hovered.is_some_and(|h| h >= rows) {
        m.hovered = None;
        m.hover.snap(0.0);
    }
    let shifted = on.is_some() && on == m.pill_on && cursor != m.pill.target;
    m.pill_on = on;
    if m.pill_in.as_ref() != Some(&here) || shifted || (cursor - m.pill.value).abs() > PILL_JUMP {
        m.pill.snap(cursor);
        m.pill_in = Some(here);
    } else {
        go(&mut m.pill, cursor, l.spring(motion::PILL));
    }

    let mut task = Task::none();
    if open != m.sheet_open {
        m.sheet_open = open;
        m.sheet.snap(0.0);
        m.backdrop = None;
        m.waiting = None;
        if open {
            if l.blur.is_some() {
                m.waiting = Some(Instant::now());
                task = window::latest()
                    .and_then(window::screenshot)
                    .map(Message::Backdrop);
            } else {
                go(&mut m.sheet, 1.0, l.bouncy(motion::SHEET));
            }
        }
    }
    task
}
