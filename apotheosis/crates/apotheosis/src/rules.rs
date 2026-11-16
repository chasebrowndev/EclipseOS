// SPDX-License-Identifier: AGPL-3.0-only
//! The rules engine (T-11 §5, AUTOMATION-12/13/14, SECURITY-24).
//!
//! * Compiled once per config load: each rule's actions are resolved against the registry,
//!   its `where` parsed, and its capability classes (macros expanded) checked against the
//!   consent table. A violation drops the rule with a diagnostic at its file and line.
//! * Firing is pure: [`Rules::fire`] takes an event and the fields it is evaluated against
//!   and returns what to run. The app runs it on the main thread, in file order, and queues
//!   the events those actions cause (non-reentrant; see `App::drain_rules`).
//! * A per-rule rate limit (default 10/s) and the app's global loop guard bound runaway
//!   chains.
//! * A rule that opted in (`allow-input` / `allow-spawn`) needs **confirm-once**: its first
//!   firing returns [`Fire::confirm`]; the answer is keyed by a hash of the rule's text so an
//!   edited rule asks again.
//!
//! Nothing here logs event fields or action arguments (they carry program-set text).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use apoth_action::event::DEFAULT_RULE_RATE_PER_SEC;
use apoth_action::{Action, Event, EventKind, MacroSet, NoContext, rule_consent};
use apoth_config::expr::{Expr, Fields};
use apoth_config::template::Template;
use apoth_config::{Config, Diagnostic, Severity};

/// Events one drain processes before the loop guard drops the rest (T-11 §5).
pub(crate) const LOOP_GUARD: usize = 256;

/// One compiled rule.
#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub(crate) name: String,
    pub(crate) event: EventKind,
    pub(crate) when: Option<Expr>,
    pub(crate) actions: Vec<Action>,
    /// `allow-input` or `allow-spawn` was set: confirm-once applies.
    pub(crate) opted_in: bool,
    /// Stable hash of the rule's text (consent key).
    pub(crate) hash: u64,
}

/// What to run for one matching rule.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fire {
    pub(crate) rule: usize,
    /// Actions with their `notify` templates rendered against the event.
    pub(crate) actions: Vec<Action>,
    /// The rule needs its confirm-once answer before these run.
    pub(crate) confirm: bool,
}

/// The confirm-once sheet's answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirm {
    /// Allow for this instance of the terminal.
    Instance,
    /// Allow always (persisted by hash).
    Always,
    /// Disable the rule.
    Disable,
}

#[derive(Debug, Default)]
pub(crate) struct Rules {
    pub(crate) rules: Vec<Rule>,
    /// Firing times inside the last second, per rule.
    recent: Vec<VecDeque<Instant>>,
    rate: u32,
    /// Hashes allowed for this instance / always / disabled.
    instance: HashSet<u64>,
    pub(crate) always: HashSet<u64>,
    disabled: HashSet<u64>,
    /// Hashes whose sheet is open (one question per rule at a time).
    asking: HashSet<u64>,
}

/// FNV-1a: stable across runs and builds (the hash is persisted).
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn diag(origin: &apoth_config::model::Origin, msg: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        file: origin.file.clone(),
        line: origin.line,
        column: origin.column,
        key: None,
        message: msg,
        suggestion: None,
    }
}

impl Rules {
    /// Compile the config's rules. Dropped rules come back as diagnostics.
    pub(crate) fn compile(cfg: &Config, macros: &MacroSet) -> (Rules, Vec<Diagnostic>) {
        let mut out = Rules {
            rate: DEFAULT_RULE_RATE_PER_SEC,
            ..Rules::default()
        };
        let mut diags = Vec::new();
        let ctx = macros.with(&NoContext);
        for r in &cfg.rules {
            let d = &r.value;
            let Some(event) = EventKind::parse(&d.event) else {
                diags.push(diag(&r.origin, format!("rule `{}`: unknown event", d.name)));
                continue;
            };
            if event.is_r2() {
                diags.push(diag(
                    &r.origin,
                    format!(
                        "rule `{}` dropped: `{}` is not available yet",
                        d.name, d.event
                    ),
                ));
                continue;
            }
            let when = match d.when.as_deref().map(Expr::parse) {
                None => None,
                Some(Ok(e)) => Some(e),
                Some(Err(m)) => {
                    diags.push(diag(
                        &r.origin,
                        format!("rule `{}`: bad `where`: {m}", d.name),
                    ));
                    continue;
                }
            };
            let Some(actions) = crate::binds::actions_of(&d.actions) else {
                diags.push(diag(
                    &r.origin,
                    format!("rule `{}` dropped: an action is not understood", d.name),
                ));
                continue;
            };
            let mut class = apoth_action::Classification::default();
            for a in &actions {
                class.merge(a.classify(&ctx));
            }
            if let Err(e) = rule_consent(&class, d.allow_input, d.allow_spawn) {
                diags.push(diag(&r.origin, format!("rule `{}` dropped: {e}", d.name)));
                continue;
            }
            for a in &actions {
                if let Action::Notify { title, body, .. } = a
                    && let Some(m) = [Some(title.as_str()), body.as_deref()]
                        .into_iter()
                        .flatten()
                        .find_map(|t| Template::parse(t).err())
                {
                    diags.push(diag(
                        &r.origin,
                        format!("rule `{}`: bad template: {m}", d.name),
                    ));
                }
            }
            let text = format!(
                "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
                d.name,
                d.event,
                d.when.as_deref().unwrap_or(""),
                actions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\u{0}"),
                d.allow_input,
                d.allow_spawn
            );
            out.rules.push(Rule {
                name: d.name.clone(),
                event,
                when,
                actions,
                opted_in: d.allow_input || d.allow_spawn,
                hash: fnv(text.as_bytes()),
            });
        }
        out.recent = vec![VecDeque::new(); out.rules.len()];
        (out, diags)
    }

    /// Keep consent answers across a reload (they are keyed by text hash, so an edited
    /// rule is asked again).
    pub(crate) fn carry_from(&mut self, old: &Rules) {
        self.instance = old.instance.clone();
        self.always = old.always.clone();
        self.disabled = old.disabled.clone();
    }

    /// True when some rule listens to `kind` (lets callers skip building fields).
    pub(crate) fn wants(&self, kind: EventKind) -> bool {
        self.rules.iter().any(|r| r.event == kind)
    }

    /// The rules that fire for `ev`, in file order.
    pub(crate) fn fire(&mut self, ev: &Event, f: &dyn Fields, now: Instant) -> Vec<Fire> {
        let mut out = Vec::new();
        for (i, r) in self.rules.iter().enumerate() {
            if r.event != ev.kind || self.disabled.contains(&r.hash) {
                continue;
            }
            if r.when.as_ref().is_some_and(|w| !w.eval(f)) {
                continue;
            }
            let q = &mut self.recent[i];
            while q
                .front()
                .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
            {
                q.pop_front();
            }
            if q.len() >= self.rate as usize {
                tracing::debug!(rule = i, "rule rate-limited");
                continue;
            }
            q.push_back(now);
            let confirmed = self.instance.contains(&r.hash) || self.always.contains(&r.hash);
            let confirm = r.opted_in && !confirmed;
            if confirm && !self.asking.insert(r.hash) {
                // Its sheet is already open: this firing is dropped, not stacked.
                continue;
            }
            out.push(Fire {
                rule: i,
                actions: r.actions.iter().map(|a| render(a, f)).collect(),
                confirm,
            });
        }
        out
    }

    /// Record the sheet's answer for rule `i`.
    pub(crate) fn answer(&mut self, i: usize, a: Confirm) {
        let Some(r) = self.rules.get(i) else { return };
        self.asking.remove(&r.hash);
        match a {
            Confirm::Instance => {
                self.instance.insert(r.hash);
            }
            Confirm::Always => {
                self.always.insert(r.hash);
            }
            Confirm::Disable => {
                self.disabled.insert(r.hash);
            }
        }
    }

    /// The sheet was dismissed without an answer (nothing runs; it asks again next time).
    pub(crate) fn dismissed(&mut self, i: usize) {
        if let Some(r) = self.rules.get(i) {
            self.asking.remove(&r.hash);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_rate(&mut self, n: u32) {
        self.rate = n;
    }
}

/// Render the `notify` templates of `a` against the event; everything else is unchanged
/// (arguments are never re-split or re-parsed, T-11 §2).
fn render(a: &Action, f: &dyn Fields) -> Action {
    match a {
        Action::Notify {
            title,
            body,
            urgency,
        } => {
            let t = |s: &str| Template::parse(s).map_or_else(|_| s.to_string(), |t| t.render(f));
            Action::Notify {
                title: t(title),
                body: body.as_deref().map(t),
                urgency: *urgency,
            }
        }
        other => other.clone(),
    }
}

/// The event queue between the app and the rules (T-11 §5): events raised while rules run
/// wait here, so no rule fires synchronously inside another. At most [`LOOP_GUARD`] events
/// wait, and one drain takes at most [`LOOP_GUARD`]; the rest are dropped and counted.
#[derive(Debug)]
pub(crate) struct Queue<T> {
    q: VecDeque<T>,
    /// Events taken in the current drain.
    taken: usize,
    pub(crate) dropped: u64,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue {
            q: VecDeque::new(),
            taken: 0,
            dropped: 0,
        }
    }
}

impl<T> Queue<T> {
    /// Queue one event; false (and counted) when the queue is full.
    pub(crate) fn push(&mut self, t: T) -> bool {
        if self.q.len() >= LOOP_GUARD {
            self.dropped += 1;
            return false;
        }
        self.q.push_back(t);
        true
    }

    /// Start a drain.
    pub(crate) fn begin(&mut self) {
        self.taken = 0;
    }

    /// The next event of this drain. Past the guard the rest are dropped: `Err(n)`.
    pub(crate) fn next(&mut self) -> Result<Option<T>, u64> {
        if self.q.is_empty() {
            return Ok(None);
        }
        if self.taken >= LOOP_GUARD {
            let n = self.q.len() as u64;
            self.q.clear();
            self.dropped += n;
            return Err(n);
        }
        self.taken += 1;
        Ok(self.q.pop_front())
    }
}

/// Fields for rules, templates and profile matches: a flat snapshot the app fills from the
/// event, the pane and the variable scopes.
#[derive(Debug, Default, Clone)]
pub(crate) struct Snapshot {
    pub(crate) text: HashMap<String, String>,
    pub(crate) states: Vec<&'static str>,
    pub(crate) vars: HashMap<String, String>,
    pub(crate) env: HashMap<String, String>,
}

impl Snapshot {
    /// Add the event's own fields (durations as milliseconds).
    pub(crate) fn with_event(mut self, ev: &Event) -> Self {
        use apoth_action::FieldValue;
        for f in ev
            .kind
            .fields()
            .iter()
            .chain(apoth_action::event::COMMON_FIELDS)
        {
            if let Some(v) = ev.get(f.name) {
                let s = match v {
                    FieldValue::Str(s) => s.clone(),
                    FieldValue::Int(n) => n.to_string(),
                    FieldValue::Bool(b) => b.to_string(),
                };
                self.text.insert(f.name.to_string(), s);
            }
        }
        self
    }
}

impl Fields for Snapshot {
    fn text(&self, field: &str) -> Option<std::borrow::Cow<'_, str>> {
        self.text.get(field).map(|s| s.as_str().into())
    }
    fn state(&self, w: &str) -> bool {
        self.states.contains(&w)
    }
    fn var(&self, n: &str) -> Option<std::borrow::Cow<'_, str>> {
        self.vars.get(n).map(|s| s.as_str().into())
    }
    fn env(&self, n: &str) -> Option<std::borrow::Cow<'_, str>> {
        self.env.get(n).map(|s| s.as_str().into())
    }
}

/// Per-scope variables set by `var.set` (T-11 §9). Pane variables also come from OSC 1337
/// (held by the term); these are the action/socket-set ones.
#[derive(Debug, Default)]
pub(crate) struct Vars {
    pub(crate) global: HashMap<String, String>,
    pub(crate) window: HashMap<String, String>,
    pub(crate) tab: HashMap<u64, HashMap<String, String>>,
    pub(crate) pane: HashMap<u64, HashMap<String, String>>,
}

/// `var.set` bounds (the action's documented limits).
pub(crate) const VAR_NAME_MAX: usize = 128;
pub(crate) const VAR_VALUE_MAX: usize = 1024;
/// Variables per scope.
pub(crate) const VARS_PER_SCOPE: usize = 64;

/// Set (or with `None`, remove) one variable in `map`. Returns true when it changed.
pub(crate) fn set_var(map: &mut HashMap<String, String>, name: &str, value: Option<&str>) -> bool {
    let name_ok = !name.is_empty()
        && name.len() <= VAR_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if !name_ok {
        return false;
    }
    match value {
        None => map.remove(name).is_some(),
        Some(v) => {
            if v.len() > VAR_VALUE_MAX || v.chars().any(char::is_control) {
                return false;
            }
            if map.get(name).map(String::as_str) == Some(v) {
                return false;
            }
            if !map.contains_key(name) && map.len() >= VARS_PER_SCOPE {
                return false;
            }
            map.insert(name.to_string(), v.to_string());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_action::{Cause, FieldValue};
    use apoth_config::fs::MemFs;
    use apoth_config::load::{LoadInput, load};

    fn cfg(text: &str) -> Config {
        let fs = MemFs::new().with("/c/a.kdl", &format!("version 1\n{text}"));
        let r = load(
            &fs,
            &LoadInput {
                user: Some("/c/a.kdl".into()),
                ..LoadInput::default()
            },
            None,
        );
        assert!(
            r.diagnostics.iter().all(|d| !d.is_error()),
            "{:?}",
            r.diagnostics
        );
        r.config
    }

    fn compile(text: &str) -> (Rules, Vec<Diagnostic>) {
        Rules::compile(&cfg(text), &MacroSet::default())
    }

    fn finished(status: i64, ms: i64) -> Event {
        let mut e = Event::new(EventKind::CommandFinished, Cause::Sequence);
        e.set("command", FieldValue::Str("make".into())).unwrap();
        e.set("status", FieldValue::Int(status)).unwrap();
        e.set("duration", FieldValue::Int(ms)).unwrap();
        e
    }

    fn snap(ev: &Event, focused: bool) -> Snapshot {
        let mut s = Snapshot::default().with_event(ev);
        if focused {
            s.states.push("focused");
        }
        s
    }

    const LONG: &str = r#"
rule "long" {
    on "command-finished"
    where "status:!0 and duration:>10s and not focused"
    do { notify title="{command}" body="exit {status}"; }
}
"#;

    /// The stock rule for `/etc/eclipse/apotheosis.kdl` (T-11 §5).
    const STOCK: &str = r#"
rule "long-command" {
    on "command-finished"
    where "duration:>=60s and not focused"
    do { notify title="{command|default:\"Command\"|truncate:60} finished" body="exit {status} in {cwd|basename}"; }
}
"#;

    #[test]
    fn where_filters_and_templates_render() {
        let (mut r, d) = compile(LONG);
        assert!(d.is_empty(), "{d:?}");
        let now = Instant::now();
        let ev = finished(2, 12_000);
        let fires = r.fire(&ev, &snap(&ev, false), now);
        assert_eq!(fires.len(), 1);
        assert!(!fires[0].confirm, "notify needs no opt-in");
        assert_eq!(
            fires[0].actions,
            vec![Action::Notify {
                title: "make".into(),
                body: Some("exit 2".into()),
                urgency: None
            }]
        );
        // Focused, a zero status or a short command: nothing.
        assert!(r.fire(&ev, &snap(&ev, true), now).is_empty());
        let ok = finished(0, 12_000);
        assert!(r.fire(&ok, &snap(&ok, false), now).is_empty());
        let short = finished(1, 2_000);
        assert!(r.fire(&short, &snap(&short, false), now).is_empty());
        // Another event kind: nothing.
        let bell = Event::new(EventKind::Bell, Cause::Child);
        assert!(r.fire(&bell, &snap(&bell, false), now).is_empty());
    }

    #[test]
    fn rules_fire_in_file_order() {
        let (mut r, _) = compile(
            r#"
rule "b" { on "bell"; do { notify title="1"; }; }
rule "a" { on "bell"; do { notify title="2"; }; }
"#,
        );
        let ev = Event::new(EventKind::Bell, Cause::Child);
        let order: Vec<usize> = r
            .fire(&ev, &snap(&ev, false), Instant::now())
            .iter()
            .map(|f| f.rule)
            .collect();
        assert_eq!(order, vec![0, 1]);
        assert_eq!(r.rules[0].name, "b");
    }

    #[test]
    fn rate_limit_per_rule() {
        let (mut r, _) = compile(r#"rule "b" { on "bell"; do { notify title="x"; }; }"#);
        r.set_rate(3);
        let ev = Event::new(EventKind::Bell, Cause::Child);
        let t0 = Instant::now();
        let n = (0..10)
            .filter(|_| !r.fire(&ev, &snap(&ev, false), t0).is_empty())
            .count();
        assert_eq!(n, 3);
        // A second later the window has moved on.
        assert_eq!(
            r.fire(&ev, &snap(&ev, false), t0 + Duration::from_secs(1))
                .len(),
            1
        );
    }

    #[test]
    fn consent_by_class_drops_rules_at_load() {
        // T-11 §10: a user-var-changed rule running a macro that wraps `launch`,
        // `capture` to a command, or `pane.split` with a command is dropped without
        // allow-spawn; input needs allow-input; read-content and config-write never.
        let text = r#"
macro "wrap" { launch type="background" { argv "true"; }; }
rule "m" { on "user-var-changed"; do { macro.run name="wrap"; }; }
rule "c" { on "user-var-changed"; do { capture source="screen" dest="command" { argv "cat"; }; }; }
rule "s" { on "user-var-changed"; do { pane.split direction="right" { argv "htop"; }; }; }
rule "i" { on "bell"; do { input.send-text text="ls"; }; }
rule "r" { on "bell"; allow-input; allow-spawn; do { var.get name="x"; }; }
rule "ok-spawn" { on "bell"; allow-spawn; do { pane.split direction="right" { argv "htop"; }; }; }
rule "ok-input" { on "bell"; allow-input; do { input.send-text text="ls"; }; }
rule "ok-layout" { on "bell"; do { pane.split direction="right"; notify title="x"; }; }
"#;
        let c = cfg(text);
        let macros = MacroSet::build(vec![apoth_action::MacroDef {
            name: "wrap".into(),
            members: vec![Action::parse(r#"launch type="background" -- true"#).unwrap()],
        }])
        .unwrap();
        let (r, d) = Rules::compile(&c, &macros);
        let kept: Vec<&str> = r.rules.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(kept, vec!["ok-spawn", "ok-input", "ok-layout"]);
        assert_eq!(d.len(), 5, "{d:?}");
        assert!(d.iter().all(|d| d.file.is_some() && d.line > 0));
        assert!(d.iter().any(|d| d.message.contains("allow-spawn")));
        assert!(d.iter().any(|d| d.message.contains("allow-input")));
        assert!(d.iter().any(|d| d.message.contains("read-content")));
        assert!(
            r.rules
                .iter()
                .find(|r| r.name == "ok-spawn")
                .unwrap()
                .opted_in
        );
        assert!(
            !r.rules
                .iter()
                .find(|r| r.name == "ok-layout")
                .unwrap()
                .opted_in
        );
    }

    #[test]
    fn confirm_once_then_answers() {
        let text = r#"rule "s" { on "bell"; allow-spawn; do { pane.split direction="right" { argv "htop"; }; }; }"#;
        let (mut r, _) = compile(text);
        let ev = Event::new(EventKind::Bell, Cause::Child);
        let t = Instant::now();
        let f = r.fire(&ev, &snap(&ev, false), t);
        assert!(f[0].confirm);
        // While its sheet is open a second firing is dropped, not stacked.
        assert!(r.fire(&ev, &snap(&ev, false), t).is_empty());
        r.answer(0, Confirm::Instance);
        assert!(!r.fire(&ev, &snap(&ev, false), t)[0].confirm);
        // A reload of the same text keeps the answer; an edit asks again.
        let (mut same, _) = compile(text);
        same.carry_from(&r);
        assert!(!same.fire(&ev, &snap(&ev, false), t)[0].confirm);
        let (mut edited, _) = compile(
            r#"rule "s" { on "bell"; allow-spawn; do { pane.split direction="left" { argv "htop"; }; }; }"#,
        );
        edited.carry_from(&r);
        assert!(edited.fire(&ev, &snap(&ev, false), t)[0].confirm);
        edited.answer(0, Confirm::Disable);
        assert!(edited.fire(&ev, &snap(&ev, false), t).is_empty());
    }

    #[test]
    fn variables_are_bounded() {
        let mut m = HashMap::new();
        assert!(set_var(&mut m, "branch", Some("main")));
        assert!(!set_var(&mut m, "branch", Some("main")), "unchanged");
        assert!(!set_var(&mut m, "bad name", Some("x")));
        assert!(!set_var(&mut m, "k", Some("a\x1bb")));
        assert!(!set_var(&mut m, "k", Some(&"x".repeat(VAR_VALUE_MAX + 1))));
        for i in 0..100 {
            set_var(&mut m, &format!("v{i}"), Some("1"));
        }
        assert_eq!(m.len(), VARS_PER_SCOPE);
        assert!(set_var(&mut m, "branch", None));
    }

    #[test]
    fn loop_guard_bounds_a_self_feeding_chain() {
        // A rule whose action raises the event it listens to: every firing queues one more.
        let (mut r, _) = compile(r#"rule "echo" { on "bell"; do { notify title="x"; }; }"#);
        r.set_rate(u32::MAX);
        let mut q: Queue<Event> = Queue::default();
        let bell = Event::new(EventKind::Bell, Cause::Child);
        assert!(q.push(bell.clone()));
        q.begin();
        let t = Instant::now();
        let mut fired = 0usize;
        let guard = loop {
            match q.next() {
                Ok(Some(ev)) => {
                    fired += r.fire(&ev, &snap(&ev, false), t).len();
                    // The action's own event goes to the back of the queue.
                    q.push(bell.clone());
                }
                Ok(None) => break None,
                Err(n) => break Some(n),
            }
        };
        assert_eq!(fired, LOOP_GUARD);
        assert_eq!(guard, Some(1));
        assert_eq!(q.dropped, 1);
        // A full queue refuses more.
        let mut q: Queue<u8> = Queue::default();
        assert_eq!(
            (0..LOOP_GUARD + 10).filter(|_| q.push(0)).count(),
            LOOP_GUARD
        );
        assert_eq!(q.dropped, 10);
    }

    #[test]
    fn default_rate_limit_is_ten_per_second() {
        let (mut r, _) = compile(r#"rule "b" { on "bell"; do { notify title="x"; }; }"#);
        let ev = Event::new(EventKind::Bell, Cause::Child);
        let t0 = Instant::now();
        let n = (0..50)
            .filter(|_| !r.fire(&ev, &snap(&ev, false), t0).is_empty())
            .count();
        assert_eq!(n, DEFAULT_RULE_RATE_PER_SEC as usize);
    }

    #[test]
    fn stock_long_command_rule_compiles_without_opt_in() {
        let (mut r, d) = compile(STOCK);
        assert!(d.is_empty(), "{d:?}");
        assert!(!r.rules[0].opted_in);
        let t = Instant::now();
        let long = finished(0, 61_000);
        let f = r.fire(&long, &snap(&long, false), t);
        assert_eq!(f.len(), 1);
        assert!(!f[0].confirm);
        assert!(r.fire(&long, &snap(&long, true), t).is_empty(), "focused");
        let short = finished(0, 59_000);
        assert!(r.fire(&short, &snap(&short, false), t).is_empty());
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
