// SPDX-License-Identifier: AGPL-3.0-only
//! The custom-widget editor's model: one `widget` block being written, and
//! the compositor's opinion of it (ADR 0065).
//!
//! Every keystroke becomes a dry-run `set_config_collection` upsert, and the
//! errors that come back are pinned to the field whose line they name, so a
//! bad interval is marked under the interval rather than in a banner.
//!
//! Commands are argv lists, one chip per argument. There is no shell anywhere
//! on this path: what the chips say is what runs, as the user.

use eclipse_ipc::{Widget, WidgetKind, WriteResult};
use serde_json::Value;

use crate::conn::Problem;

/// The data sources a `source` widget can read, as the compositor names them.
pub const SOURCES: [&str; 7] = [
    "usage.cpu",
    "usage.mem",
    "usage.gpu",
    "usage.disk",
    "audio.volume",
    "media.title",
    "media.artist",
];

/// The compositor's own defaults for a new block.
pub const DEFAULT_INTERVAL_MS: u32 = 5000;
pub const DEFAULT_FORMAT: &str = "{}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Run a command every interval; its output is the reading.
    Exec,
    /// Run a command once; every line it prints is a new reading.
    Stream,
    /// Read a shipped data source through a format string.
    Source,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Exec, Kind::Stream, Kind::Source];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Exec => "exec",
            Kind::Stream => "stream",
            Kind::Source => "source",
        }
    }

    /// One line on what the kind does, under the picker.
    pub fn gist(self) -> &'static str {
        match self {
            Kind::Exec => "Runs the command on an interval; its output is the reading.",
            Kind::Stream => "Runs the command once; each line it prints replaces the reading.",
            Kind::Source => "Reads a built-in source through a format string. Nothing runs.",
        }
    }
}

/// The argv lists the editor holds, one chip row each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Command,
    Click,
    ScrollUp,
    ScrollDown,
}

impl Slot {
    pub const ACTIONS: [Slot; 3] = [Slot::Click, Slot::ScrollUp, Slot::ScrollDown];

    fn index(self) -> usize {
        match self {
            Slot::Command => 0,
            Slot::Click => 1,
            Slot::ScrollUp => 2,
            Slot::ScrollDown => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Slot::Command => "command",
            Slot::Click => "on click",
            Slot::ScrollUp => "on scroll up",
            Slot::ScrollDown => "on scroll down",
        }
    }

    pub fn field(self) -> Field {
        match self {
            Slot::Command => Field::Command,
            Slot::Click => Field::Click,
            Slot::ScrollUp => Field::ScrollUp,
            Slot::ScrollDown => Field::ScrollDown,
        }
    }
}

/// Which input an error belongs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Name,
    Command,
    Interval,
    Source,
    Format,
    Icon,
    Click,
    ScrollUp,
    ScrollDown,
    /// Nothing to pin it to: shown at the foot of the editor.
    Block,
}

/// One refusal, positioned.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldError {
    pub field: Field,
    /// `file:line:col`, file reduced to its name.
    pub position: String,
    pub message: String,
    pub snippet: String,
    /// 1-based, into `snippet`.
    pub col: usize,
    pub span: usize,
}

/// A chip row and the text being typed at its end.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Argv {
    pub args: Vec<String>,
    pub draft: String,
}

impl Argv {
    fn from(v: &[String]) -> Argv {
        Argv {
            args: v.to_vec(),
            draft: String::new(),
        }
    }

    /// The list as it would be written: a half-typed argument counts, so
    /// what is validated is what is on screen.
    pub fn resolved(&self) -> Vec<String> {
        let mut v = self.args.clone();
        if !self.draft.is_empty() {
            v.push(self.draft.clone());
        }
        v
    }

    /// Enter: the draft becomes a chip.
    pub fn push(&mut self) {
        if !self.draft.is_empty() {
            self.args.push(std::mem::take(&mut self.draft));
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    /// The block's name when the editor opened; `None` for a new one.
    pub original: Option<String>,
    pub name: String,
    pub kind: Kind,
    argvs: [Argv; 4],
    pub interval: String,
    pub source: String,
    pub format: String,
    pub icon: String,
    pub errors: Vec<FieldError>,
    /// Whether a dry run has answered for the current text.
    pub checked: bool,
    /// Set by a refused save: shown until the next edit.
    pub refused: Option<String>,
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new()
    }
}

impl Editor {
    pub fn new() -> Editor {
        Editor {
            original: None,
            name: String::new(),
            kind: Kind::Exec,
            argvs: Default::default(),
            interval: DEFAULT_INTERVAL_MS.to_string(),
            source: SOURCES[0].to_owned(),
            format: DEFAULT_FORMAT.to_owned(),
            icon: String::new(),
            errors: Vec::new(),
            checked: false,
            refused: None,
        }
    }

    pub fn from_widget(w: &Widget) -> Editor {
        let mut e = Editor::new();
        e.original = Some(w.name.clone());
        e.name = w.name.clone();
        match &w.kind {
            WidgetKind::Exec { argv, interval_ms } => {
                e.kind = Kind::Exec;
                e.argvs[0] = Argv::from(argv);
                e.interval = interval_ms.to_string();
            }
            WidgetKind::Stream { argv } => {
                e.kind = Kind::Stream;
                e.argvs[0] = Argv::from(argv);
            }
            WidgetKind::Source { source, format } => {
                e.kind = Kind::Source;
                e.source = source.clone();
                e.format = format.clone();
            }
        }
        e.icon = w.icon.clone().unwrap_or_default();
        for (slot, v) in Slot::ACTIONS
            .into_iter()
            .zip([&w.on_click, &w.on_scroll_up, &w.on_scroll_down])
        {
            if let Some(v) = v {
                e.argvs[slot.index()] = Argv::from(v);
            }
        }
        e
    }

    pub fn argv(&self, slot: Slot) -> &Argv {
        &self.argvs[slot.index()]
    }

    pub fn argv_mut(&mut self, slot: Slot) -> &mut Argv {
        &mut self.argvs[slot.index()]
    }

    pub fn is_new(&self) -> bool {
        self.original.is_none()
    }

    pub fn renamed(&self) -> bool {
        self.original.as_deref().is_some_and(|o| o != self.name.trim())
    }

    /// The block as it would be written. `Err` is a refusal the editor can
    /// make on its own, pinned to its field, before asking the compositor.
    pub fn widget(&self) -> Result<Widget, FieldError> {
        let local = |field, message: &str| FieldError {
            field,
            position: String::new(),
            message: message.to_owned(),
            snippet: String::new(),
            col: 0,
            span: 0,
        };
        let name = self.name.trim();
        if name.is_empty() {
            return Err(local(Field::Name, "A widget needs a name."));
        }
        let kind =
            match self.kind {
                Kind::Exec => WidgetKind::Exec {
                    argv: self.argv(Slot::Command).resolved(),
                    interval_ms: self.interval.trim().parse().map_err(|_| {
                        local(Field::Interval, "The interval is a whole number of milliseconds.")
                    })?,
                },
                Kind::Stream => WidgetKind::Stream {
                    argv: self.argv(Slot::Command).resolved(),
                },
                Kind::Source => WidgetKind::Source {
                    source: self.source.clone(),
                    format: self.format.clone(),
                },
            };
        let action = |slot: Slot| {
            let v = self.argv(slot).resolved();
            (!v.is_empty()).then_some(v)
        };
        Ok(Widget {
            name: name.to_owned(),
            kind,
            icon: (!self.icon.trim().is_empty()).then(|| self.icon.trim().to_owned()),
            on_click: action(Slot::Click),
            on_scroll_up: action(Slot::ScrollUp),
            on_scroll_down: action(Slot::ScrollDown),
        })
    }

    /// Take a dry run's verdict.
    pub fn take_result(&mut self, r: &WriteResult) {
        self.errors = r.errors.iter().map(parse_error).collect();
        if !r.valid && self.errors.is_empty() {
            self.errors.push(FieldError {
                field: Field::Block,
                position: String::new(),
                message: "The compositor would not load this block.".into(),
                snippet: String::new(),
                col: 0,
                span: 0,
            });
        }
        self.checked = true;
    }

    /// Take a refusal that came back as an RPC error instead of a verdict.
    pub fn take_problem(&mut self, p: &Problem) {
        self.errors = vec![match p {
            Problem::Other(text) => parse_text(text),
            other => FieldError {
                field: Field::Block,
                position: other.headline(),
                message: other.detail().to_owned(),
                snippet: String::new(),
                col: 0,
                span: 0,
            },
        }];
        self.checked = true;
    }

    pub fn errors_for(&self, field: Field) -> impl Iterator<Item = &FieldError> {
        self.errors.iter().filter(move |e| e.field == field)
    }

    pub fn valid(&self) -> bool {
        self.checked && self.errors.is_empty()
    }

    /// Format `{}` with a stand-in reading, so the format field shows what
    /// the bar will.
    pub fn format_preview(&self) -> String {
        let sample = match self.source.as_str() {
            "usage.cpu" | "usage.mem" | "usage.gpu" | "usage.disk" => "42",
            "audio.volume" => "62",
            "media.title" => "Midnight City",
            "media.artist" => "M83",
            _ => "42",
        };
        self.format.replace("{}", sample)
    }
}

/// Which field a snippet's line belongs to, by the KDL node it starts with.
pub fn field_of(snippet: &str, message: &str) -> Field {
    let head = snippet.split_whitespace().next().unwrap_or_default();
    let by_node = |node: &str| match node {
        "exec" | "stream" => Some(Field::Command),
        "interval-ms" => Some(Field::Interval),
        "source" => Some(Field::Source),
        "format" => Some(Field::Format),
        "icon" => Some(Field::Icon),
        "on-click" => Some(Field::Click),
        "on-scroll-up" => Some(Field::ScrollUp),
        "on-scroll-down" => Some(Field::ScrollDown),
        "widget" => Some(Field::Name),
        _ => None,
    };
    if let Some(f) = by_node(head) {
        return f;
    }
    // No snippet: look for a node name in the message instead. Longest
    // first, so `on-scroll-up` is not read as `source`.
    for node in [
        "on-scroll-down",
        "on-scroll-up",
        "on-click",
        "interval-ms",
        "format",
        "source",
        "icon",
        "exec",
    ] {
        if message.contains(node) {
            return by_node(node).unwrap_or(Field::Block);
        }
    }
    Field::Block
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One `ConfigError` object: `{file, line, col, message, snippet, spanLen}`.
pub fn parse_error(v: &Value) -> FieldError {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
    let (message, snippet) = (s("message"), s("snippet"));
    let (line, col) = (n("line"), n("col"));
    let file = s("file");
    FieldError {
        field: field_of(&snippet, &message),
        position: if line == 0 {
            file_name(&file).to_owned()
        } else {
            format!("{}:{line}:{col}", file_name(&file))
        },
        // The snippet is shown trimmed, so the column is moved with it.
        col: col.saturating_sub(leading(&snippet)),
        snippet: snippet.trim().to_owned(),
        span: n("spanLen"),
        message,
    }
}

fn leading(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}

/// `file:line:col: message`, the text shape an `invalid_params` refusal
/// carries. Anything else is kept whole as the message.
pub fn parse_text(text: &str) -> FieldError {
    let whole = || FieldError {
        field: field_of("", text),
        position: String::new(),
        message: text.to_owned(),
        snippet: String::new(),
        col: 0,
        span: 0,
    };
    let Some((pos, message)) = text.split_once(": ") else {
        return whole();
    };
    let mut parts = pos.rsplitn(3, ':');
    let (Some(col), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        return whole();
    };
    let (Ok(line), Ok(col)) = (line.parse::<usize>(), col.parse::<usize>()) else {
        return whole();
    };
    FieldError {
        field: field_of("", message),
        position: format!("{}:{line}:{col}", file_name(file)),
        message: message.to_owned(),
        snippet: String::new(),
        col,
        span: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn weather() -> Widget {
        Widget {
            name: "weather".into(),
            kind: WidgetKind::Exec {
                argv: vec!["curl".into(), "-s".into(), "wttr.in/?format=%t".into()],
                interval_ms: 600_000,
            },
            icon: Some("weather-clear-symbolic".into()),
            on_click: Some(vec!["xdg-open".into(), "https://wttr.in".into()]),
            on_scroll_up: None,
            on_scroll_down: None,
        }
    }

    #[test]
    fn a_widget_round_trips_through_the_editor() {
        let w = weather();
        assert_eq!(Editor::from_widget(&w).widget(), Ok(w));
    }

    #[test]
    fn a_half_typed_argument_is_part_of_the_command() {
        let mut e = Editor::new();
        e.name = "up".into();
        e.argv_mut(Slot::Command).args = vec!["uptime".into()];
        e.argv_mut(Slot::Command).draft = "-p".into();
        let WidgetKind::Exec { argv, .. } = e.widget().unwrap().kind else {
            panic!("exec")
        };
        assert_eq!(argv, ["uptime", "-p"]);
    }

    #[test]
    fn an_argument_with_spaces_stays_one_argument() {
        let mut a = Argv {
            draft: "hello world".into(),
            ..Argv::default()
        };
        a.push();
        assert_eq!(a.args, ["hello world"]);
    }

    #[test]
    fn local_refusals_are_pinned_to_their_field() {
        let mut e = Editor::new();
        assert_eq!(e.widget().unwrap_err().field, Field::Name);
        e.name = "x".into();
        e.interval = "soon".into();
        assert_eq!(e.widget().unwrap_err().field, Field::Interval);
    }

    #[test]
    fn a_verdict_is_pinned_by_the_node_on_its_line() {
        let v = json!({
            "file": "/home/u/.config/eclipse/abyss.kdl", "line": 41, "col": 9,
            "message": "interval-ms must be at least 250",
            "snippet": "    interval-ms 10", "spanLen": 2
        });
        let e = parse_error(&v);
        assert_eq!(e.field, Field::Interval);
        assert_eq!(e.position, "abyss.kdl:41:9");
        assert_eq!(e.snippet, "interval-ms 10");
        assert_eq!(e.col, 5, "the caret moves with the trimmed snippet");
        assert_eq!(e.span, 2);
    }

    #[test]
    fn scroll_up_is_not_read_as_source() {
        assert_eq!(field_of("", "on-scroll-up needs a command"), Field::ScrollUp);
        assert_eq!(field_of("", "unknown source \"x\""), Field::Source);
        assert_eq!(field_of("", "something else"), Field::Block);
    }

    #[test]
    fn a_text_refusal_keeps_its_position() {
        let e = parse_text("/etc/abyss.kdl:3:14: format must contain {}");
        assert_eq!(e.position, "abyss.kdl:3:14");
        assert_eq!(e.message, "format must contain {}");
        assert_eq!(e.field, Field::Format);
        assert_eq!(parse_text("no position here").message, "no position here");
    }

    #[test]
    fn the_format_preview_fills_the_placeholder() {
        let mut e = Editor::new();
        e.kind = Kind::Source;
        e.format = "CPU {}%".into();
        assert_eq!(e.format_preview(), "CPU 42%");
    }
}
