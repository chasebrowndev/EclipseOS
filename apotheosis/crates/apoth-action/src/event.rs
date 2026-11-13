// SPDX-License-Identifier: Apache-2.0
//! The closed event vocabulary for rules (T-11 §5, AUTOMATION-13), with the field names
//! templates and the match language read, and the trusted/program-triggered flag.
//!
//! The dispatcher and the per-rule rate limit (default 10/s) live in `apotheosis`.

use std::fmt;

use crate::arg::MAX_TEXT_BYTES;
use crate::error::{ErrorKind, ParseError};

/// Default per-rule rate limit, events per second (T-11 §5).
pub const DEFAULT_RULE_RATE_PER_SEC: u32 = 10;
/// Longest string event field (T-18 §5 budget; longer values are refused, not truncated).
pub const MAX_FIELD_BYTES: usize = 4096;

/// The type of an event field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Text.
    Str,
    /// Signed integer (`status`, `pid`, sizes).
    Int,
    /// Milliseconds (`duration`).
    Duration,
    /// Flag.
    Bool,
}

/// One named field an event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventField {
    /// Name as used in templates (`{status}`) and `where` (`status:!0`).
    pub name: &'static str,
    /// Type.
    pub ty: FieldType,
    /// True when the value can carry program-set text (content-bearing, T-12 §4).
    pub content: bool,
}

const fn f(name: &'static str, ty: FieldType, content: bool) -> EventField {
    EventField { name, ty, content }
}

use FieldType::{Bool, Duration, Int, Str};

/// Fields every event carries (ids of where it happened).
pub const COMMON_FIELDS: &[EventField] = &[
    f("pane", Int, false),
    f("tab", Int, false),
    f("window", Int, false),
];

const F_RESIZE: &[EventField] = &[f("cols", Int, false), f("rows", Int, false)];
const F_TITLE: &[EventField] = &[f("title", Str, true)];
const F_CWD: &[EventField] = &[f("cwd", Str, true)];
const F_HOST: &[EventField] = &[f("host", Str, true), f("user", Str, true)];
const F_FOREGROUND: &[EventField] = &[f("process", Str, true), f("pid", Int, false)];
const F_STARTED: &[EventField] = &[f("command", Str, true), f("cwd", Str, true)];
const F_FINISHED: &[EventField] = &[
    f("command", Str, true),
    f("status", Int, false),
    f("duration", Duration, false),
    f("cwd", Str, true),
];
const F_IDLE: &[EventField] = &[f("duration", Duration, false)];
const F_VAR: &[EventField] = &[f("name", Str, true), f("value", Str, true)];
const F_PROGRESS: &[EventField] = &[f("state", Str, false), f("percent", Int, false)];
const F_NOTIFICATION: &[EventField] = &[f("title", Str, true), f("body", Str, true)];
const F_RELOADED: &[EventField] = &[f("ok", Bool, false)];
const F_THEME: &[EventField] = &[f("scheme", Str, false)];
const F_MEDIA_SOURCE: &[EventField] = &[f("source", Str, true)];
const F_MEDIA_ERROR: &[EventField] = &[f("message", Str, true)];
const F_MEDIA_POSITION: &[EventField] = &[f("position", Duration, false)];
const F_MATCH: &[EventField] = &[f("line", Str, true), f("match", Str, true)];

crate::string_enum! {
    /// The closed event set.
    EventKind {
        Bell = "bell",
        FocusIn = "focus-in",
        FocusOut = "focus-out",
        Resize = "resize",
        TitleChanged = "title-changed",
        CwdChanged = "cwd-changed",
        HostChanged = "host-changed",
        ForegroundChanged = "foreground-changed",
        PromptStart = "prompt-start",
        CommandStarted = "command-started",
        CommandFinished = "command-finished",
        OutputIdle = "output-idle",
        OutputActivity = "output-activity",
        PaneOpened = "pane-opened",
        PaneClosed = "pane-closed",
        TabOpened = "tab-opened",
        TabClosed = "tab-closed",
        WindowOpened = "window-opened",
        WindowClosed = "window-closed",
        UserVarChanged = "user-var-changed",
        ProgressChanged = "progress-changed",
        Notification = "notification",
        ConfigReloaded = "config-reloaded",
        ThemeChanged = "theme-changed",
        MediaStarted = "media-started",
        MediaEnded = "media-ended",
        MediaError = "media-error",
        MediaPosition = "media-position",
        Startup = "startup",
        OutputMatch = "output-match",
    }
}

impl EventKind {
    /// Member of the trusted set (T-11 §5): trusted only when caused by the user or the
    /// user's config. See [`Event::program_triggered`].
    pub const fn in_trusted_set(self) -> bool {
        matches!(
            self,
            EventKind::Startup
                | EventKind::ConfigReloaded
                | EventKind::ThemeChanged
                | EventKind::FocusIn
                | EventKind::FocusOut
                | EventKind::Resize
                | EventKind::PaneOpened
                | EventKind::PaneClosed
                | EventKind::TabOpened
                | EventKind::TabClosed
                | EventKind::WindowOpened
                | EventKind::WindowClosed
        )
    }

    /// True for events that exist only from R2 on.
    pub const fn is_r2(self) -> bool {
        matches!(self, EventKind::OutputMatch)
    }

    /// Fields beyond [`COMMON_FIELDS`].
    pub const fn fields(self) -> &'static [EventField] {
        match self {
            EventKind::Resize => F_RESIZE,
            EventKind::TitleChanged => F_TITLE,
            EventKind::CwdChanged => F_CWD,
            EventKind::HostChanged => F_HOST,
            EventKind::ForegroundChanged => F_FOREGROUND,
            EventKind::CommandStarted => F_STARTED,
            EventKind::CommandFinished => F_FINISHED,
            EventKind::OutputIdle => F_IDLE,
            EventKind::UserVarChanged => F_VAR,
            EventKind::ProgressChanged => F_PROGRESS,
            EventKind::Notification => F_NOTIFICATION,
            EventKind::ConfigReloaded => F_RELOADED,
            EventKind::ThemeChanged => F_THEME,
            EventKind::MediaStarted | EventKind::MediaEnded => F_MEDIA_SOURCE,
            EventKind::MediaError => F_MEDIA_ERROR,
            EventKind::MediaPosition => F_MEDIA_POSITION,
            EventKind::OutputMatch => F_MATCH,
            EventKind::Bell
            | EventKind::FocusIn
            | EventKind::FocusOut
            | EventKind::PromptStart
            | EventKind::OutputActivity
            | EventKind::PaneOpened
            | EventKind::PaneClosed
            | EventKind::TabOpened
            | EventKind::TabClosed
            | EventKind::WindowOpened
            | EventKind::WindowClosed
            | EventKind::Startup => &[],
        }
    }

    /// Look up a field by name among the event's own and the common fields.
    pub fn field(self, name: &str) -> Option<EventField> {
        self.fields()
            .iter()
            .chain(COMMON_FIELDS.iter())
            .copied()
            .find(|x| x.name == name)
    }
}

/// What caused an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A real seat action by the user.
    User,
    /// The user's config (startup rules, reload, layouts).
    Config,
    /// An escape sequence from a program in a pane.
    Sequence,
    /// A control-socket client.
    Socket,
    /// A child process's own behaviour (exit, output, bell).
    Child,
}

/// A field value. `Debug` never prints string content (command lines, titles).
#[derive(Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// Text.
    Str(String),
    /// Integer or milliseconds.
    Int(i64),
    /// Flag.
    Bool(bool),
}

/// An event instance. `Debug` prints the kind, cause and field names only.
#[derive(Clone, PartialEq, Eq)]
pub struct Event {
    /// Which event.
    pub kind: EventKind,
    /// What caused it.
    pub cause: Cause,
    fields: Vec<(&'static str, FieldValue)>,
}

impl fmt::Debug for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldValue::Str(s) => write!(f, "Str(<redacted {} bytes>)", s.len()),
            FieldValue::Int(n) => write!(f, "Int({n})"),
            FieldValue::Bool(b) => write!(f, "Bool({b})"),
        }
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.fields.iter().map(|(n, _)| *n).collect();
        f.debug_struct("Event")
            .field("kind", &self.kind)
            .field("cause", &self.cause)
            .field("fields", &names)
            .finish()
    }
}

impl Event {
    /// A new event with no fields set.
    pub fn new(kind: EventKind, cause: Cause) -> Self {
        Self {
            kind,
            cause,
            fields: Vec::new(),
        }
    }

    /// Set a field; the name must exist for this kind, the value must match its type,
    /// and strings obey the byte budget.
    pub fn set(&mut self, name: &str, value: FieldValue) -> Result<(), ParseError> {
        let Some(spec) = self.kind.field(name) else {
            return Err(ParseError::new(
                ErrorKind::UnknownParam,
                format!("event `{}` has no field `{name}`", self.kind.as_str()),
            ));
        };
        let ok = matches!(
            (&spec.ty, &value),
            (FieldType::Str, FieldValue::Str(_))
                | (FieldType::Int | FieldType::Duration, FieldValue::Int(_))
                | (FieldType::Bool, FieldValue::Bool(_))
        );
        if !ok {
            return Err(ParseError::new(
                ErrorKind::BadValue,
                format!("field `{name}` has the wrong type"),
            ));
        }
        if let FieldValue::Str(s) = &value
            && s.len() > MAX_FIELD_BYTES.min(MAX_TEXT_BYTES)
        {
            return Err(ParseError::new(
                ErrorKind::BadValue,
                format!(
                    "field `{name}` is {} bytes, limit is {MAX_FIELD_BYTES}",
                    s.len()
                ),
            ));
        }
        self.fields.retain(|(n, _)| *n != spec.name);
        self.fields.push((spec.name, value));
        Ok(())
    }

    /// Read a field.
    pub fn get(&self, name: &str) -> Option<&FieldValue> {
        self.fields.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    /// Program-triggered unless the kind is in the trusted set *and* the cause is the user
    /// or the user's config (T-11 §5: a socket-caused event is program-triggered, and so
    /// is anything an escape sequence provoked).
    pub fn program_triggered(&self) -> bool {
        !(self.kind.in_trusted_set() && matches!(self.cause, Cause::User | Cause::Config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_string_content() {
        let mut e = Event::new(EventKind::parse("command-finished").unwrap(), Cause::User);
        e.set("command", FieldValue::Str("curl -H secret-token".into()))
            .unwrap();
        let shown = format!("{e:?} {:?}", e.get("command").unwrap());
        assert!(!shown.contains("secret-token"), "{shown}");
        assert!(shown.contains("command"));
    }

    #[test]
    fn names_roundtrip_and_are_unique() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(*k));
        }
        let mut names: Vec<&str> = EventKind::NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), EventKind::ALL.len());
    }

    #[test]
    fn spec_list_is_complete() {
        for n in [
            "bell",
            "focus-in",
            "focus-out",
            "resize",
            "title-changed",
            "cwd-changed",
            "host-changed",
            "foreground-changed",
            "prompt-start",
            "command-started",
            "command-finished",
            "output-idle",
            "output-activity",
            "pane-opened",
            "pane-closed",
            "tab-opened",
            "tab-closed",
            "window-opened",
            "window-closed",
            "user-var-changed",
            "progress-changed",
            "notification",
            "config-reloaded",
            "theme-changed",
            "startup",
            "output-match",
        ] {
            assert!(EventKind::parse(n).is_some(), "{n}");
        }
        assert!(
            EventKind::NAMES
                .iter()
                .filter(|n| n.starts_with("media-"))
                .count()
                >= 1
        );
        assert!(EventKind::parse("consent-answered").is_none());
    }

    #[test]
    fn trusted_set_is_exactly_the_spec_list() {
        let trusted: Vec<&str> = EventKind::ALL
            .iter()
            .filter(|k| k.in_trusted_set())
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            trusted,
            vec![
                "focus-in",
                "focus-out",
                "resize",
                "pane-opened",
                "pane-closed",
                "tab-opened",
                "tab-closed",
                "window-opened",
                "window-closed",
                "config-reloaded",
                "theme-changed",
                "startup"
            ]
        );
        assert_eq!(trusted.len(), 12);
        for n in [
            "bell",
            "title-changed",
            "user-var-changed",
            "notification",
            "output-match",
        ] {
            assert!(!EventKind::parse(n).unwrap().in_trusted_set());
        }
    }

    #[test]
    fn program_triggered_depends_on_cause() {
        let e = Event::new(EventKind::Startup, Cause::Config);
        assert!(!e.program_triggered());
        let e = Event::new(EventKind::FocusIn, Cause::User);
        assert!(!e.program_triggered());
        let e = Event::new(EventKind::FocusIn, Cause::Socket);
        assert!(e.program_triggered());
        let e = Event::new(EventKind::PaneOpened, Cause::Sequence);
        assert!(e.program_triggered());
        let e = Event::new(EventKind::ThemeChanged, Cause::Child);
        assert!(e.program_triggered());
        for cause in [
            Cause::User,
            Cause::Config,
            Cause::Sequence,
            Cause::Socket,
            Cause::Child,
        ] {
            assert!(Event::new(EventKind::Bell, cause).program_triggered());
            assert!(Event::new(EventKind::CommandFinished, cause).program_triggered());
        }
    }

    #[test]
    fn fields_typed_and_budgeted() {
        let mut e = Event::new(EventKind::CommandFinished, Cause::Child);
        e.set("status", FieldValue::Int(2)).unwrap();
        e.set("duration", FieldValue::Int(12_000)).unwrap();
        e.set("command", FieldValue::Str("make".into())).unwrap();
        e.set("pane", FieldValue::Int(7)).unwrap();
        assert_eq!(e.get("status"), Some(&FieldValue::Int(2)));
        e.set("status", FieldValue::Int(0)).unwrap();
        assert_eq!(e.get("status"), Some(&FieldValue::Int(0)));
        assert!(e.set("status", FieldValue::Str("x".into())).is_err());
        assert!(e.set("nope", FieldValue::Int(1)).is_err());
        let big = "x".repeat(MAX_FIELD_BYTES + 1);
        assert!(e.set("command", FieldValue::Str(big)).is_err());
        assert!(
            e.set("command", FieldValue::Str("x".repeat(MAX_FIELD_BYTES)))
                .is_ok()
        );
    }

    #[test]
    fn every_kind_has_distinct_field_names() {
        for k in EventKind::ALL {
            let mut names: Vec<&str> = k.fields().iter().map(|x| x.name).collect();
            names.extend(COMMON_FIELDS.iter().map(|x| x.name));
            let n = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), n, "{}", k.as_str());
        }
    }
}
