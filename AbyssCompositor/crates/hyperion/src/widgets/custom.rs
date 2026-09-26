// SPDX-License-Identifier: AGPL-3.0-only
//! A `widget "<name>" { … }` block: an icon and a line of text as the core,
//! the first detail line as the revealed section.
//!
//! The text is a command's output and therefore **untrusted**: it is drawn as
//! plain text (iced has no markup to inject), cut to one line with control
//! characters dropped, and it is never logged — not here, not on failure.
//! The `on-click`/`on-scroll-*` argv are the user's own config and run through
//! the custom service, never a shell.
//!
//! A `source` block runs nothing: it is resolved here, from the state the
//! other widgets already hold ([`resolve`]). A block with an icon and an
//! empty line shows the icon alone; one with neither takes no room.

use iced::mouse::ScrollDelta;
use iced::widget::{mouse_area, text, Row};
use iced::Alignment;

use eclipse_services::custom::{Output, WidgetSpec};
use eclipse_ui::tokens::{bar, color, font, size};
use eclipse_ui::widget::{self as parts, ShellFrame};

use super::{Action, Feed as Routed, Parts, Spans};
use crate::app::Message;

#[derive(Debug, Default)]
pub struct State {
    /// The last good output. A failure clears it: stale output presented as
    /// current is worse than none.
    pub out: Option<Output>,
}

#[derive(Debug, Clone)]
pub enum Feed {
    Output(Output),
    Failed,
    /// Run one of the block's own argv.
    Run(Vec<String>),
}

pub fn update(state: &mut State, feed: Feed) -> Option<Action> {
    match feed {
        Feed::Output(o) => state.out = Some(o),
        Feed::Failed => state.out = None,
        Feed::Run(argv) => return (!argv.is_empty()).then_some(Action::Run(argv)),
    }
    None
}

/// The most text a custom cell will hold.
const MAX_CHARS: usize = (bar::MEDIA_TEXT_W / bar::CHAR_W) as usize;

/// One printable line of untrusted output, at most [`MAX_CHARS`].
fn line(s: &str) -> String {
    let first = s.lines().next().unwrap_or_default();
    let clean: String = first.chars().filter(|c| !c.is_control()).collect();
    parts::elide(clean.trim(), MAX_CHARS)
}

/// The width a line of text takes, within the cell's bounds.
fn text_w(s: &str) -> f32 {
    (s.chars().count() as f32 * bar::CHAR_W).clamp(bar::MARK, bar::MEDIA_TEXT_W)
}

fn icon_w(spec: &WidgetSpec) -> f32 {
    if spec.icon.is_some() {
        bar::MARK + bar::WIDGET_GAP
    } else {
        0.0
    }
}

fn detail(out: &Output) -> Option<String> {
    out.detail.first().map(|d| line(d)).filter(|d| !d.is_empty())
}

/// A `source` block's line: the shipped source's reading through `format`
/// (`{}` is the value). `None` while the source has nothing to say.
pub fn resolve(source: &str, format: &str, widgets: &super::State) -> Option<String> {
    let pct = |f: f32| format!("{:.0}%", f * 100.0);
    let sample = widgets.usage.sample.as_ref();
    let np = widgets.now_playing.player.as_ref();
    let value = match source {
        "usage.cpu" => sample.map(|s| pct(s.cpu)),
        "usage.mem" => sample.and_then(|s| s.mem).map(pct),
        "usage.gpu" => sample.and_then(|s| s.gpu).map(pct),
        "usage.disk" => sample.and_then(|s| s.disk).map(pct),
        "audio.volume" => widgets.volume.sink.as_ref().map(|s| {
            if s.muted {
                "muted".to_owned()
            } else {
                pct(s.volume)
            }
        }),
        "media.title" => np.map(|p| p.title.clone()),
        "media.artist" => np.and_then(|p| p.artist.clone()),
        _ => None,
    }?;
    Some(format.replace("{}", &value))
}

pub fn spans(out: Option<&Output>, spec: &WidgetSpec) -> Spans {
    let Some(out) = out else {
        return Spans::default();
    };
    let body = line(&out.text);
    if body.is_empty() && spec.icon.is_none() {
        return Spans::default();
    }
    let text = if body.is_empty() { 0.0 } else { text_w(&body) };
    Spans {
        core: (icon_w(spec) + text).max(bar::MARK),
        revealed: detail(out).map_or(0.0, |d| text_w(&d)),
        present: true,
    }
}

/// The block's icon: a path if it looks like one, else a theme name.
fn icon(name: &str) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        Some(std::path::PathBuf::from(name))
    } else {
        crate::icons::symbolic(name)
    }
}

fn msg(name: &str, f: Feed) -> Message {
    Message::Widget(Routed::Custom(name.to_owned(), f))
}

pub fn view<'a>(out: Option<Output>, spec: &'a WidgetSpec, _frame: ShellFrame) -> Parts<'a> {
    let Some(out) = out else {
        return Parts::empty();
    };
    let ink = if out.state.as_deref() == Some("critical") {
        color::DANGER
    } else {
        color::TEXT_SECONDARY
    };
    let mut core = Row::new().spacing(bar::WIDGET_GAP).align_y(Alignment::Center);
    if let Some(name) = &spec.icon {
        core = core.push(parts::mark(icon(name), bar::MARK, ink));
    }
    let body = line(&out.text);
    if !body.is_empty() {
        core = core.push(
            text(body)
                .font(font::DATA)
                .size(size::MONO)
                .color(ink)
                .wrapping(text::Wrapping::None),
        );
    }
    let mut area = mouse_area(core);
    if let Some(argv) = &spec.on_click {
        area = area.on_press(msg(&spec.name, Feed::Run(argv.clone())));
    }
    if spec.on_scroll_up.is_some() || spec.on_scroll_down.is_some() {
        let (name, up, down) = (
            spec.name.clone(),
            spec.on_scroll_up.clone().unwrap_or_default(),
            spec.on_scroll_down.clone().unwrap_or_default(),
        );
        area = area.on_scroll(move |d| {
            let dy = match d {
                ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => y,
            };
            let argv = if dy > 0.0 { up.clone() } else { down.clone() };
            msg(&name, Feed::Run(argv))
        });
    }
    let revealed = detail(&out).map(|d| {
        text(d)
            .font(font::DATA)
            .size(size::MICRO)
            .color(color::TEXT_TERTIARY)
            .wrapping(text::Wrapping::None)
            .into()
    });
    Parts {
        core: area.into(),
        revealed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eclipse_services::custom::Kind;

    fn spec() -> WidgetSpec {
        WidgetSpec {
            name: "weather".into(),
            kind: Kind::Stream {
                argv: vec!["x".into()],
            },
            icon: None,
            on_click: None,
            on_scroll_up: None,
            on_scroll_down: None,
        }
    }

    fn out(text: &str) -> Output {
        Output {
            text: text.into(),
            detail: Vec::new(),
            tooltip: None,
            state: None,
        }
    }

    /// Untrusted output is one printable line, bounded, whatever it sent.
    #[test]
    fn output_is_one_bounded_printable_line() {
        let s = line("18°C\x1b[31m red\nsecond line");
        assert!(!s.contains('\n'));
        assert!(!s.chars().any(char::is_control));
        assert!(line(&"x".repeat(500)).chars().count() <= MAX_CHARS);
    }

    #[test]
    fn empty_or_failed_output_takes_no_room() {
        let mut st = State::default();
        update(&mut st, Feed::Output(out("   ")));
        assert!(!spans(st.out.as_ref(), &spec()).present);
        let iconic = WidgetSpec {
            icon: Some("weather-clear".into()),
            ..spec()
        };
        assert!(
            spans(st.out.as_ref(), &iconic).present,
            "an icon alone still shows"
        );
        update(&mut st, Feed::Output(out("18°C")));
        assert!(spans(st.out.as_ref(), &spec()).present);
        update(&mut st, Feed::Failed);
        assert!(!spans(st.out.as_ref(), &spec()).present);
    }

    #[test]
    fn a_source_reads_the_other_widgets_state() {
        let mut w = super::super::State::default();
        assert_eq!(resolve("usage.cpu", "{}", &w), None);
        w.usage.sample = Some(eclipse_services::usage::Sample {
            cpu: 0.42,
            mem: None,
            gpu: None,
            disk: None,
        });
        assert_eq!(resolve("usage.cpu", "CPU {}", &w).as_deref(), Some("CPU 42%"));
        assert_eq!(resolve("usage.mem", "{}", &w), None);
    }

    #[test]
    fn an_empty_argv_runs_nothing() {
        assert_eq!(update(&mut State::default(), Feed::Run(Vec::new())), None);
    }
}
