// SPDX-License-Identifier: AGPL-3.0-only
//! System usage: CPU and memory meters as the core, GPU and disk as the
//! revealed section — each a labelled fraction over a hairline track, never
//! a bare number.
//!
//! Neutral throughout: a load reading is a status, and the bar's accent is
//! for the live value.

use std::path::PathBuf;

use iced::widget::Row;
use iced::Alignment;

use eclipse_services::usage::Sample;
use eclipse_ui::tokens::bar;
use eclipse_ui::widget::{self as parts, ShellFrame};

use super::{Action, Parts, Spans};

/// `bar.widgets.system-usage.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cfg {
    pub interval_ms: u64,
    pub gpu: bool,
    pub disk: bool,
    pub disk_path: PathBuf,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            interval_ms: 1000,
            gpu: true,
            disk: true,
            disk_path: PathBuf::from("/"),
        }
    }
}

#[derive(Debug, Default)]
pub struct State {
    /// The last reading; `None` until the sampler's first, and no widget
    /// until then.
    pub sample: Option<Sample>,
}

#[derive(Debug, Clone)]
pub enum Feed {
    Sample(Sample),
}

pub fn update(state: &mut State, feed: Feed) -> Option<Action> {
    match feed {
        Feed::Sample(s) => state.sample = Some(s),
    }
    None
}

/// The meters a reading draws, core first: `(label, fraction, revealed)`.
fn meters(s: &Sample, cfg: &Cfg) -> Vec<(&'static str, f32, bool)> {
    let mut out = vec![("cpu", s.cpu, false)];
    if let Some(m) = s.mem {
        out.push(("mem", m, false));
    }
    if let (true, Some(g)) = (cfg.gpu, s.gpu) {
        out.push(("gpu", g, true));
    }
    if let (true, Some(d)) = (cfg.disk, s.disk) {
        out.push(("disk", d, true));
    }
    out
}

/// `n` meters side by side.
fn run(n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * bar::METER_W + (n - 1) as f32 * bar::WIDGET_GAP
    }
}

pub fn spans(state: &State, cfg: &Cfg) -> Spans {
    let Some(s) = state.sample.as_ref() else {
        return Spans::default();
    };
    let m = meters(s, cfg);
    let revealed = m.iter().filter(|(_, _, r)| *r).count();
    Spans {
        core: run(m.len() - revealed),
        revealed: run(revealed),
        present: true,
    }
}

pub fn view<'a>(state: &'a State, cfg: &Cfg, frame: ShellFrame) -> Parts<'a> {
    let Some(s) = state.sample.as_ref() else {
        return Parts::empty();
    };
    let row = || Row::new().spacing(bar::WIDGET_GAP).align_y(Alignment::Center);
    let (mut core, mut more, mut any) = (row(), row(), false);
    for (label, f, revealed) in meters(s, cfg) {
        let ink = if revealed {
            frame.revealed_alpha()
        } else {
            frame.core_alpha()
        };
        let meter = parts::mini_meter_faded(label, f, false, ink);
        if revealed {
            more = more.push(meter);
            any = true;
        } else {
            core = core.push(meter);
        }
    }
    Parts {
        core: core.into(),
        revealed: any.then(|| more.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_source_takes_no_room() {
        let mut s = State::default();
        assert!(!spans(&s, &Cfg::default()).present);
        update(
            &mut s,
            Feed::Sample(Sample {
                cpu: 0.4,
                mem: Some(0.5),
                gpu: None,
                disk: Some(0.7),
            }),
        );
        let sp = spans(&s, &Cfg::default());
        assert_eq!(sp.core, run(2));
        assert_eq!(sp.revealed, run(1));
        let off = Cfg {
            disk: false,
            ..Cfg::default()
        };
        assert_eq!(spans(&s, &off).revealed, 0.0);
    }
}
