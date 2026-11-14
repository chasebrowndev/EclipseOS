// SPDX-License-Identifier: AGPL-3.0-only
//! Replies (bytes for the PTY) and the per-pane event slot (T-00 §4.3, T-18 §5).

use std::collections::VecDeque;

/// Reply queue bound per pane (T-18 §5).
pub const REPLY_CAP: usize = 64 * 1024;
/// Marks kept before dropping (and counting).
pub const MARKS_CAP: usize = 256;
/// Notifications kept before dropping.
pub const NOTIFY_CAP: usize = 16;
/// Clipboard sets kept before dropping.
pub const CLIP_CAP: usize = 4;
/// User-variable changes and command transitions queued per drain.
pub const VARS_CAP: usize = 64;
pub const COMMANDS_CAP: usize = 64;

/// Whole-unit reply queue; the io thread is its producer, main its consumer.
#[derive(Debug, Default)]
pub struct ReplyQueue {
    units: VecDeque<Vec<u8>>,
    bytes: usize,
    dropped: u64,
}

impl ReplyQueue {
    pub(crate) fn push(&mut self, unit: Vec<u8>) {
        if self.bytes + unit.len() > REPLY_CAP {
            self.dropped += 1;
            return;
        }
        self.bytes += unit.len();
        self.units.push_back(unit);
    }
    /// Oldest unit.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let u = self.units.pop_front()?;
        self.bytes -= u.len();
        Some(u)
    }
    /// All pending bytes, concatenated in order.
    pub fn take_all(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes);
        while let Some(u) = self.pop() {
            out.extend_from_slice(&u);
        }
        out
    }
    /// Nothing pending.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
    /// Bytes pending.
    pub fn len_bytes(&self) -> usize {
        self.bytes
    }
    /// Replies dropped on overflow.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// OSC 133 mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// `A`, `B`, `C` or `D`.
    pub kind: char,
    /// Exit status of `D`.
    pub exit: Option<i32>,
    /// Active-area row at the time.
    pub row: u32,
    /// Column at the time.
    pub col: u32,
}

/// OSC 9;4 progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// 0 remove, 1 value, 2 error, 3 indeterminate, 4 paused.
    pub state: u8,
    /// 0..=100.
    pub value: u8,
}

/// An OSC 52 write, tagged program-origin for the paste guard (T-18 §4(e)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardSet {
    /// Selection letters as sent (filtered to `cpqs0-7`).
    pub selection: String,
    /// Decoded bytes (empty clears).
    pub data: Vec<u8>,
    /// Always true: the content came from a program.
    pub program_origin: bool,
}

/// `OSC 1337 ; SetUserVar=name=<base64>` (T-11 §9): `value` is `None` when the variable
/// was removed (empty value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserVar {
    pub name: String,
    pub value: Option<String>,
}

/// A program notification (T-17 §6). The text is program content: shown, never logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The OSC number it came from: 9, 777 or 99.
    pub source: u16,
    /// Sanitized text (`title: body` for OSC 777).
    pub text: String,
}

/// A command block changed state (T-14 §1): it started running, or it finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandEvent {
    /// The block id ([`crate::shell::Blocks::get`]).
    pub block: u32,
    pub finished: bool,
}

/// Counters of things dropped (never content).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// Marks dropped on overflow.
    pub marks: u64,
    /// Notifications dropped.
    pub notifications: u64,
    /// Clipboard sets dropped on overflow or policy.
    pub clipboard: u64,
    /// OSC 52 / 1337 payloads with invalid base64.
    pub bad_base64: u64,
    /// User-variable changes dropped (queue full or too many names).
    pub vars: u64,
    /// Command events dropped on overflow.
    pub commands: u64,
}

/// The fixed per-pane slot: latest-wins title, cwd, progress; a bell counter; bounded
/// queues for marks, notifications and clipboard sets.
#[derive(Debug)]
pub struct EventSlot {
    /// New title pending.
    pub title: Option<String>,
    /// New working directory (OSC 7 URI) pending.
    pub cwd: Option<String>,
    /// Latest progress.
    pub progress: Option<Progress>,
    /// Bells since the last drain.
    pub bell: u32,
    /// OSC 133 marks.
    pub marks: VecDeque<Mark>,
    /// Notifications (OSC 9, 777, 99), with the sequence they came from.
    pub notifications: VecDeque<Notification>,
    /// OSC 52 writes.
    pub clipboard: VecDeque<ClipboardSet>,
    /// A pending OSC 52 read request (selection letters, filtered); latest wins. The term
    /// never answers it: main decides under the sequence policy (T-18 §3: ask locally,
    /// deny remotely) and, when allowed, queues the base64 reply itself.
    pub clip_read: Option<String>,
    /// The palette or default colours changed.
    pub colors_changed: bool,
    /// OSC 1337 user-variable changes.
    pub vars: VecDeque<UserVar>,
    /// Command started / finished transitions.
    pub commands: VecDeque<CommandEvent>,
    /// File-type image media wait for consent (T-18 §3 `ask`): the request token. Main
    /// answers with [`crate::Term::graphics_file_decision`]; the pane's output is held
    /// until then.
    pub graphics_file: Option<u64>,
    /// Drop counters.
    pub dropped: Dropped,
}

impl Default for EventSlot {
    fn default() -> Self {
        EventSlot {
            title: None,
            cwd: None,
            progress: None,
            bell: 0,
            marks: VecDeque::with_capacity(MARKS_CAP),
            notifications: VecDeque::with_capacity(NOTIFY_CAP),
            clipboard: VecDeque::with_capacity(CLIP_CAP),
            clip_read: None,
            colors_changed: false,
            vars: VecDeque::with_capacity(VARS_CAP),
            commands: VecDeque::with_capacity(COMMANDS_CAP),
            graphics_file: None,
            dropped: Dropped::default(),
        }
    }
}

impl EventSlot {
    /// Nothing pending.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.cwd.is_none()
            && self.progress.is_none()
            && self.bell == 0
            && self.marks.is_empty()
            && self.notifications.is_empty()
            && self.clipboard.is_empty()
            && self.clip_read.is_none()
            && !self.colors_changed
            && self.vars.is_empty()
            && self.commands.is_empty()
            && self.graphics_file.is_none()
    }
    /// Forget everything pending (drop counters persist).
    pub fn clear(&mut self) {
        self.title = None;
        self.cwd = None;
        self.progress = None;
        self.bell = 0;
        self.marks.clear();
        self.notifications.clear();
        self.clipboard.clear();
        self.clip_read = None;
        self.colors_changed = false;
        self.vars.clear();
        self.commands.clear();
        self.graphics_file = None;
    }
    pub(crate) fn push_mark(&mut self, m: Mark) {
        if self.marks.len() >= MARKS_CAP {
            self.dropped.marks += 1;
        } else {
            self.marks.push_back(m);
        }
    }
    pub(crate) fn push_var(&mut self, v: UserVar) {
        if self.vars.len() >= VARS_CAP {
            self.dropped.vars += 1;
        } else {
            self.vars.push_back(v);
        }
    }
    pub(crate) fn push_command(&mut self, c: CommandEvent) {
        if self.commands.len() >= COMMANDS_CAP {
            self.dropped.commands += 1;
        } else {
            self.commands.push_back(c);
        }
    }
    pub(crate) fn push_notification(&mut self, source: u16, text: String) {
        if self.notifications.len() >= NOTIFY_CAP {
            self.dropped.notifications += 1;
        } else {
            self.notifications.push_back(Notification { source, text });
        }
    }
    pub(crate) fn push_clipboard(&mut self, c: ClipboardSet) {
        if self.clipboard.len() >= CLIP_CAP {
            self.dropped.clipboard += 1;
        } else {
            self.clipboard.push_back(c);
        }
    }
    /// Canonical text form, one line per pending event, for fixtures. Order: title, cwd,
    /// progress, bell, marks, notifications, clipboard, clipboard read, colors.
    pub fn render(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(t) = &self.title {
            v.push(format!("title {t}"));
        }
        if let Some(c) = &self.cwd {
            v.push(format!("cwd {c}"));
        }
        if let Some(p) = self.progress {
            v.push(format!("progress {} {}", p.state, p.value));
        }
        if self.bell > 0 {
            v.push(format!("bell {}", self.bell));
        }
        for m in &self.marks {
            match m.exit {
                Some(e) => v.push(format!("mark {} exit={} row={}", m.kind, e, m.row)),
                None => v.push(format!("mark {} row={}", m.kind, m.row)),
            }
        }
        for n in &self.notifications {
            v.push(format!("notify {}", n.text));
        }
        for c in &self.clipboard {
            v.push(format!(
                "clip {} {}",
                c.selection,
                c.data
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ));
        }
        if let Some(s) = &self.clip_read {
            v.push(format!("clip-read {s}"));
        }
        if self.colors_changed {
            v.push("colors".to_string());
        }
        for u in &self.vars {
            match &u.value {
                Some(val) => v.push(format!("var {}={val}", u.name)),
                None => v.push(format!("var {} unset", u.name)),
            }
        }
        for c in &self.commands {
            let what = if c.finished { "finished" } else { "started" };
            v.push(format!("command {} {what}", c.block));
        }
        if self.graphics_file.is_some() {
            v.push("graphics-file".to_string());
        }
        v
    }
}
