// SPDX-License-Identifier: AGPL-3.0-only
//! Automation on the main thread: the rules hub (T-11 §5), scoped variables (T-11 §9),
//! program notifications and progress (T-17 §6), automatic profile matching (T-11 §6) and
//! the title templates (T-11 §8).
//!
//! Every event the app raises goes through [`App::ctl_emit`], which also queues it here when
//! some rule listens to its kind. The queue drains on an idle callback, in order and never
//! reentrantly: events the rules' own actions raise are queued behind, and one drain stops
//! after [`LOOP_GUARD`] events (the rest are dropped and counted). Nothing here logs event
//! fields, variable values, notification text or action arguments.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use apoth_action::enums::{Scope, Urgency};
use apoth_action::{Action, Cause, Event, EventKind, FieldValue};
use apoth_config::Diagnostic;

use super::App;
use crate::notify::{self, Note, RateGate, when_allows};
use crate::pane::{CommandInfo, PaneEvents, PaneKey};
use crate::profiles::{self, Context, Switch};
use crate::rules::{Confirm, Queue, Rules, Snapshot, Vars, set_var};

/// Persisted "allow always" answers: at most this many hashes, this many bytes read.
const CONSENT_MAX: usize = 1024;
const CONSENT_FILE_MAX: u64 = 64 * 1024;

/// Program-set user variables kept per pane (mirrors the term's, T-11 §9 bounds).
const PANE_VARS_MAX: usize = apoth_term::USER_VARS_MAX;

/// A rule waiting for its confirm-once answer (T-11 §5).
pub(super) struct PendingRule {
    pub(super) rule: usize,
    pub(super) actions: Vec<Action>,
}

/// Automation state owned by the app.
#[derive(Default)]
pub(super) struct Auto {
    pub(super) rules: Rules,
    queue: Queue<(Event, Option<PaneKey>)>,
    draining: bool,
    scheduled: bool,
    vars: Vars,
    gate: RateGate,
    pub(super) confirm: Option<PendingRule>,
    /// Program-set (OSC 1337) variables per pane.
    pane_vars: HashMap<PaneKey, BTreeMap<String, String>>,
    /// The host a pane's OSC 7 last named (local panes: this host).
    hosts: HashMap<PaneKey, String>,
    /// Automatic profile per pane, and the explicit one a pane was started with.
    switches: HashMap<PaneKey, Switch>,
    explicit: HashMap<PaneKey, String>,
}

/// `$XDG_STATE_HOME/apotheosis/rule-consent` (else `~/.local/state/…`).
fn consent_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("apotheosis/rule-consent"))
}

/// Parse the consent file: one 16-digit hex hash per line, anything else ignored.
pub(super) fn parse_consent(text: &str) -> HashSet<u64> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            (l.len() == 16)
                .then(|| u64::from_str_radix(l, 16).ok())
                .flatten()
        })
        .take(CONSENT_MAX)
        .collect()
}

pub(super) fn render_consent(set: &HashSet<u64>) -> String {
    let mut v: Vec<u64> = set.iter().copied().collect();
    v.sort_unstable();
    v.truncate(CONSENT_MAX);
    v.iter().map(|h| format!("{h:016x}\n")).collect()
}

/// Read the persisted answers (called once before the loop starts, like the config load).
pub(super) fn load_consent() -> HashSet<u64> {
    use std::io::Read;
    let Some(p) = consent_path() else {
        return HashSet::new();
    };
    let Ok(f) = std::fs::File::open(p) else {
        return HashSet::new();
    };
    let mut s = String::new();
    if f.take(CONSENT_FILE_MAX).read_to_string(&mut s).is_err() {
        return HashSet::new();
    }
    parse_consent(&s)
}

/// The confirm-once sheet (T-11 §5): the rule's name and trigger, then the fully expanded
/// actions in a bounded monospace box (program text, sanitised); explicit keys only, no
/// Return and no Escape button (Escape dismisses). A spec with a body draws as a modal sheet.
pub(super) fn rule_bar_spec(
    id: apoth_ui::WidgetId,
    name: &str,
    event: &str,
    actions: &str,
) -> apoth_ui::bar::BarSpec {
    use apoth_ui::bar::{BarKind, BarSpec, ButtonSpec, Key as BarKey};
    use apoth_ui::text::TextRun;
    use apoth_ui::tokens::size;
    let button = |label: &str, c: char| ButtonSpec {
        label: label.into(),
        key: BarKey::Char(c),
        primary: false,
    };
    BarSpec {
        id,
        kind: BarKind::Consent,
        headline: TextRun::interface(
            format!("Allow rule \u{201c}{}\u{201d}?", super::clip(name, 128)),
            size::BODY,
        )
        .strong(),
        detail: Some(TextRun::interface(
            format!("Triggered by {event}. When allowed it will run:"),
            size::BODY_SMALL,
        )),
        buttons: vec![
            button("Allow this time", 'o'),
            button("Always allow", 'a'),
            button("Disable rule", 'd'),
        ],
        escape: None,
        body: Some(TextRun::program(
            super::clip(actions, 512),
            size::BODY_SMALL,
        )),
    }
}

fn urgency_byte(u: Option<Urgency>) -> u8 {
    match u {
        Some(Urgency::Low) => 0,
        Some(Urgency::Critical) => 2,
        Some(Urgency::Normal) | None => 1,
    }
}

/// The automatic-match context of a pane (user and foreground job are not tracked yet).
fn ctx_of(host: Option<&str>, cwd: Option<&std::path::Path>) -> Context {
    Context {
        host: host.map(str::to_string),
        user: None,
        path: cwd.map(|p| p.to_string_lossy().into_owned()),
        job: None,
    }
}

impl App {
    // ----------------------------------------------------------------- rules

    /// Compile the rules of the live config (startup and every applied reload). Consent
    /// answers carry over by rule hash; dropped rules come back as diagnostics.
    pub(super) fn recompile_rules(&mut self) -> Vec<Diagnostic> {
        let (mut r, diags) = Rules::compile(self.reloader.config(), &self.keymap.macros);
        r.carry_from(&self.auto.rules);
        if !r.rules.is_empty() {
            tracing::info!(rules = r.rules.len(), dropped = diags.len(), "rules loaded");
        }
        self.auto.rules = r;
        // A sheet for a rule that is gone or renumbered is withdrawn.
        if self.auto.confirm.take().is_some() {
            self.refresh_bar();
        }
        diags
    }

    /// Queue `ev` for the rules (called by `ctl_emit` with the common fields set).
    pub(super) fn auto_enqueue(&mut self, ev: &Event, pane: Option<PaneKey>) {
        if !self.auto.rules.wants(ev.kind) {
            return;
        }
        if !self.auto.queue.push((ev.clone(), pane)) {
            return;
        }
        if !self.auto.scheduled {
            self.auto.scheduled = true;
            self.lh.insert_idle(|app: &mut App| app.auto_drain());
        }
    }

    /// Run the queued events through the rules, in order, non-reentrantly.
    pub(super) fn auto_drain(&mut self) {
        self.auto.scheduled = false;
        if self.auto.draining {
            return;
        }
        self.auto.draining = true;
        self.auto.queue.begin();
        loop {
            let (ev, pane) = match self.auto.queue.next() {
                Ok(Some(x)) => x,
                Ok(None) => break,
                Err(lost) => {
                    tracing::warn!(
                        dropped = lost,
                        total = self.auto.queue.dropped,
                        "rule loop guard: events dropped"
                    );
                    break;
                }
            };
            let snap = self.snapshot(&ev, pane);
            let fires = self.auto.rules.fire(&ev, &snap, Instant::now());
            for f in fires {
                let actions = self.filter_rule_actions(&ev, f.actions);
                if f.confirm {
                    if self.bar_busy() {
                        // One question on screen at a time: it asks again next time.
                        self.auto.rules.dismissed(f.rule);
                        continue;
                    }
                    self.auto.confirm = Some(PendingRule {
                        rule: f.rule,
                        actions,
                    });
                    self.bar_dismissed = false;
                    self.refresh_bar();
                    continue;
                }
                self.dispatch(actions, 0);
            }
        }
        self.auto.draining = false;
    }

    /// `notifications.min-duration` drops a `notify` a `command-finished` rule runs for a
    /// shorter command (T-17 §6).
    fn filter_rule_actions(&self, ev: &Event, actions: Vec<Action>) -> Vec<Action> {
        let min = self.settings.notify.min_duration.as_millis() as i64;
        let short = ev.kind == EventKind::CommandFinished
            && matches!(ev.get("duration"), Some(FieldValue::Int(d)) if *d < min);
        if !short {
            return actions;
        }
        actions
            .into_iter()
            .filter(|a| !matches!(a, Action::Notify { .. }))
            .collect()
    }

    /// Some interface question is on screen (paste, consent, link, rule).
    pub(super) fn bar_busy(&self) -> bool {
        self.paste.is_some()
            || self.nav.consent.is_some()
            || self.nav.open.is_some()
            || self.auto.confirm.is_some()
    }

    /// The rule sheet's answer: 0 allow for this instance, 1 allow always, 2 disable; Escape
    /// dismisses (nothing runs).
    pub(super) fn answer_rule(&mut self, answer: Option<usize>) {
        let Some(p) = self.auto.confirm.take() else {
            return;
        };
        self.refresh_bar();
        let a = match answer {
            Some(0) => Confirm::Instance,
            Some(1) => Confirm::Always,
            Some(2) => Confirm::Disable,
            _ => {
                self.auto.rules.dismissed(p.rule);
                return;
            }
        };
        self.auto.rules.answer(p.rule, a);
        if a == Confirm::Always {
            self.save_consent();
        }
        if a != Confirm::Disable {
            self.dispatch(p.actions, 0);
        }
    }

    /// Persist the "allow always" hashes on a worker (never blocks the loop).
    fn save_consent(&mut self) {
        let Some(path) = consent_path() else { return };
        let text = render_consent(&self.auto.rules.always);
        let job = move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                return;
            }
            tracing::warn!("cannot save rule consent");
        };
        if self.workers.submit(Box::new(job)).is_err() {
            tracing::warn!("worker queue full; rule consent not saved");
        }
    }

    /// The rule sheet (interim: the stock consent bar; the glass sheet is frontend work).
    pub(super) fn rule_bar(&self) -> Option<apoth_ui::bar::BarSpec> {
        let p = self.auto.confirm.as_ref()?;
        let rule = self.auto.rules.rules.get(p.rule)?;
        let what: Vec<String> = p.actions.iter().map(ToString::to_string).collect();
        Some(rule_bar_spec(
            self.windows()[0],
            &rule.name,
            rule.event.as_str(),
            &what.join("; "),
        ))
    }

    /// The fields rules, templates and matches see for `pane` (or the focused pane).
    pub(super) fn snapshot(&self, ev: &Event, pane: Option<PaneKey>) -> Snapshot {
        let mut s = Snapshot::default().with_event(ev);
        let Some(id) = pane.or_else(|| self.focused()) else {
            s.vars = self.auto.vars.global.clone();
            return s;
        };
        self.pane_fields(id, &mut s);
        s
    }

    /// Pane fields (`title`, `cwd`, `host`, `profile`, ids), states and variables. Fields the
    /// event already carries win.
    pub(super) fn pane_fields(&self, id: PaneKey, s: &mut Snapshot) {
        let tab = self.session.tab_of(id);
        let mut put = |k: &str, v: String| {
            s.text.entry(k.to_string()).or_insert(v);
        };
        put("pane", id.to_string());
        put("id", id.to_string());
        put("window", self.mux_win.to_string());
        if let Some(t) = tab {
            put("tab", t.to_string());
        }
        if let Some(p) = self.panes.get(&id) {
            put("title", p.title.clone());
            if let Some(c) = &p.cwd {
                put("cwd", c.to_string_lossy().into_owned());
            }
            if p.exit.is_none() {
                s.states.push("running");
            }
        }
        put(
            "host",
            self.auto
                .hosts
                .get(&id)
                .cloned()
                .unwrap_or_else(|| self.hostname.clone()),
        );
        if let Some(pr) = self.pane_profile(id) {
            put("profile", pr.to_string());
        }
        if self.focused() == Some(id) {
            s.states.push("active");
            if self.win_focused {
                s.states.push("focused");
            }
        }
        if tab
            .and_then(|t| self.session.tab(t))
            .is_some_and(|t| t.zoomed().is_some())
        {
            s.states.push("zoomed");
        }
        // Most specific last: global, window, tab, pane (`var.set`), then program-set.
        let mut vars = self.auto.vars.global.clone();
        vars.extend(self.auto.vars.window.clone());
        if let Some(m) = tab.and_then(|t| self.auto.vars.tab.get(&t)) {
            vars.extend(m.clone());
        }
        if let Some(m) = self.auto.vars.pane.get(&id) {
            vars.extend(m.clone());
        }
        if let Some(m) = self.auto.pane_vars.get(&id) {
            vars.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        s.vars = vars;
    }

    /// The profile in force for a pane: the automatic one, else the explicit one.
    pub(super) fn pane_profile(&self, id: PaneKey) -> Option<&str> {
        self.auto
            .switches
            .get(&id)
            .and_then(|s| s.current.as_deref())
            .or_else(|| self.auto.explicit.get(&id).map(String::as_str))
    }

    /// The window title from the `window.title` template and the focused pane.
    pub(super) fn window_title_now(&self) -> String {
        let mut s = Snapshot::default();
        s.text.insert("title".into(), self.term_title.clone());
        if let Some(id) = self.focused() {
            self.pane_fields(id, &mut s);
        }
        self.settings.window_title_with(&s)
    }

    /// A tab's label from the `tabs.title` template (the pane title alone when it is the
    /// stock `{title}`).
    pub(super) fn tab_label(&self, pane: Option<PaneKey>, title: &str) -> String {
        let mut s = Snapshot::default();
        s.text.insert("title".into(), title.to_string());
        if let Some(id) = pane {
            self.pane_fields(id, &mut s);
        }
        self.settings.tab_title_with(&s)
    }

    // ------------------------------------------------------------- variables

    /// `var.set` / `var.unset` on the focused pane's scope. Raises `user-var-changed`.
    pub(super) fn var_action(&mut self, name: &str, value: Option<&str>, scope: Option<Scope>) {
        let pane = self.focused();
        let tab = self.active_tab();
        let map = match scope.unwrap_or(Scope::Pane) {
            Scope::Global => &mut self.auto.vars.global,
            Scope::Window => &mut self.auto.vars.window,
            Scope::Tab => match tab {
                Some(t) => self.auto.vars.tab.entry(t).or_default(),
                None => return,
            },
            Scope::Pane => match pane {
                Some(p) => self.auto.vars.pane.entry(p).or_default(),
                None => return,
            },
        };
        if set_var(map, name, value) {
            self.emit_var(pane, name, value, Cause::User);
        }
    }

    fn emit_var(&mut self, pane: Option<PaneKey>, name: &str, value: Option<&str>, cause: Cause) {
        let mut ev = Event::new(EventKind::UserVarChanged, cause);
        let _ = ev.set("name", FieldValue::Str(name.to_string()));
        let _ = ev.set(
            "value",
            FieldValue::Str(value.unwrap_or_default().to_string()),
        );
        self.ctl_emit(ev, pane);
    }

    /// A pane went: drop its automation state.
    pub(super) fn auto_forget(&mut self, id: PaneKey) {
        self.auto.vars.pane.remove(&id);
        self.auto.pane_vars.remove(&id);
        self.auto.hosts.remove(&id);
        self.auto.switches.remove(&id);
        self.auto.explicit.remove(&id);
        self.auto.gate.forget(id);
    }

    /// A tab went: drop its variables.
    pub(super) fn auto_forget_tab(&mut self, tab: u64) {
        self.auto.vars.tab.remove(&tab);
    }

    // -------------------------------------------------------------- profiles

    /// A pane started with an explicit profile (`profile=`, `--profile`).
    pub(super) fn set_explicit_profile(&mut self, id: PaneKey, name: &str) {
        self.auto.explicit.insert(id, name.to_string());
    }

    /// Shell, cwd and env of `profile` for a new pane; `Err` names an unknown profile.
    /// Explicit `argv` / `cwd` win over the profile's.
    #[allow(clippy::type_complexity)]
    pub(super) fn profile_launch(
        &self,
        profile: &str,
        argv: Option<Vec<String>>,
        cwd: Option<PathBuf>,
    ) -> Result<(Option<Vec<String>>, Option<PathBuf>, Vec<(String, String)>), ()> {
        let Some(sp) = profiles::spawn_of(self.reloader.config(), profile) else {
            return Err(());
        };
        let argv = argv.or_else(|| {
            sp.program.map(|p| {
                let mut v = vec![p];
                v.extend(sp.args);
                v
            })
        });
        let cwd = cwd.or_else(|| sp.cwd.map(PathBuf::from));
        Ok((argv, cwd, sp.env))
    }

    /// Re-run automatic matching for a pane (cwd or host changed, T-11 §6).
    fn rematch(&mut self, id: PaneKey) {
        if self.auto.explicit.contains_key(&id) {
            // An explicit choice is not switched away from automatically.
            return;
        }
        let cfg = self.reloader.config();
        if !cfg.profiles.iter().any(|p| !p.value.matches.is_empty()) {
            return;
        }
        let host = self.auto.hosts.get(&id).map(String::as_str);
        let cwd = self.panes.get(&id).and_then(|p| p.cwd.as_deref());
        let c = ctx_of(host.or(Some(&self.hostname)), cwd);
        let next = profiles::choose(cfg.profiles.iter().map(|p| &p.value), &c)
            .map(|i| cfg.profiles[i].value.name.clone());
        let sw = self.auto.switches.entry(id).or_default();
        let now = |k: &str| cfg.get(k).cloned().unwrap_or(apoth_config::Value::Null);
        let set = sw.retarget(cfg, next.as_deref(), &now);
        if !set.is_empty() {
            tracing::debug!(pane = id, keys = set.len(), "profile switched");
            // HOOK(T-11 §6): appearance keys apply per pane once render takes per-pane
            // appearance (apoth-render); until then the switch only changes the `profile`
            // field rules and templates see.
        }
    }

    // ------------------------------------------------------- pane events

    /// The automation part of a pane's events: notifications, progress, prompt and command
    /// marks, user variables, host changes. Called from `on_events`.
    pub(super) fn auto_pane_events(&mut self, id: PaneKey, e: &mut PaneEvents, uri: Option<&str>) {
        if let Some(uri) = uri {
            let host = uri
                .strip_prefix("file://")
                .and_then(|r| r.split('/').next())
                .filter(|h| !h.is_empty() && *h != "localhost")
                .unwrap_or(&self.hostname)
                .to_string();
            let host = super::clip(&host, 255).to_owned();
            let old = self.auto.hosts.insert(id, host.clone());
            if old.as_deref().unwrap_or(&self.hostname) != host {
                let mut ev = Event::new(EventKind::HostChanged, Cause::Sequence);
                let _ = ev.set("host", FieldValue::Str(host));
                self.ctl_emit(ev, Some(id));
            }
            self.rematch(id);
        }
        for _ in 0..e.prompts.min(8) {
            self.ctl_emit(
                Event::new(EventKind::PromptStart, Cause::Sequence),
                Some(id),
            );
        }
        for c in std::mem::take(&mut e.commands) {
            self.emit_command(id, c);
        }
        for v in std::mem::take(&mut e.vars) {
            let m = self.auto.pane_vars.entry(id).or_default();
            match &v.value {
                Some(val) => {
                    if m.len() < PANE_VARS_MAX || m.contains_key(&v.name) {
                        m.insert(v.name.clone(), val.clone());
                    }
                }
                None => {
                    m.remove(&v.name);
                }
            }
            self.emit_var(Some(id), &v.name, v.value.as_deref(), Cause::Sequence);
        }
        if let Some(p) = e.progress.take() {
            // The tab's progress line (T-08 §4) follows the pane's latest report.
            if p.state == 0 {
                self.status.progress.remove(&id);
            } else {
                self.status.progress.insert(id, (p.state, p.value));
            }
            self.send_layout();
            let state = match p.state {
                0 => "remove",
                1 => "value",
                2 => "error",
                3 => "indeterminate",
                _ => "paused",
            };
            let mut ev = Event::new(EventKind::ProgressChanged, Cause::Sequence);
            let _ = ev.set("state", FieldValue::Str(state.into()));
            let _ = ev.set("percent", FieldValue::Int(i64::from(p.value)));
            self.ctl_emit(ev, Some(id));
        }
        for n in std::mem::take(&mut e.notifications) {
            self.program_notification(id, n);
        }
    }

    fn emit_command(&mut self, id: PaneKey, c: CommandInfo) {
        let kind = if c.finished {
            EventKind::CommandFinished
        } else {
            EventKind::CommandStarted
        };
        let mut ev = Event::new(kind, Cause::Sequence);
        if let Some(cmd) = c.cmdline {
            let _ = ev.set("command", FieldValue::Str(cmd));
        }
        if let Some(cwd) = c.cwd {
            let cwd = crate::ctl::osc7::local_path(&cwd, &self.hostname)
                .map_or(cwd, |p| p.to_string_lossy().into_owned());
            let _ = ev.set("cwd", FieldValue::Str(cwd));
        }
        if c.finished {
            let _ = ev.set("status", FieldValue::Int(i64::from(c.exit.unwrap_or(0))));
            let ms = c.duration_ms.unwrap_or(0).min(i64::MAX as u64) as i64;
            let _ = ev.set("duration", FieldValue::Int(ms));
        }
        self.ctl_emit(ev, Some(id));
    }

    /// OSC 9 / 777 / 99: raise `notification`, then deliver under the filters.
    fn program_notification(&mut self, id: PaneKey, n: apoth_term::Notification) {
        if !self.settings.notify.accepts(n.source) {
            return;
        }
        let title = self
            .panes
            .get(&id)
            .map(|p| p.title.clone())
            .unwrap_or_default();
        let note = Note::from_osc(n.source, &n.text, &title);
        let mut ev = Event::new(EventKind::Notification, Cause::Sequence);
        let _ = ev.set("title", FieldValue::Str(note.title.clone()));
        let _ = ev.set("body", FieldValue::Str(note.body.clone()));
        self.ctl_emit(ev, Some(id));
        self.deliver_for(id, note);
    }

    /// A bell with `bell.notify` on.
    pub(super) fn bell_notification(&mut self, id: PaneKey) {
        if !self.settings.notify.bell {
            return;
        }
        let title = self
            .panes
            .get(&id)
            .map(|p| p.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Apotheosis".into());
        self.deliver_for(id, Note::new(&title, "Bell", 1));
    }

    /// Deliver a pane's notification if `notifications.when` and the rate allow it.
    fn deliver_for(&mut self, id: PaneKey, note: Note) {
        let focused = self.win_focused && self.focused() == Some(id);
        let visible = self.placed.iter().any(|p| p.id == id);
        if !when_allows(self.settings.notify.when, focused, visible) {
            return;
        }
        if !self
            .auto
            .gate
            .admit(id, self.settings.notify.per_minute, Instant::now())
        {
            tracing::debug!(pane = id, "notification rate-limited");
            return;
        }
        self.deliver(note);
    }

    /// The `notify` action: the user's (or a rule's) own notification. Rate-limited as one
    /// source; `notifications.when` does not apply (the user asked for it).
    pub(super) fn notify_action(
        &mut self,
        title: &str,
        body: Option<&str>,
        urgency: Option<Urgency>,
    ) {
        if !self
            .auto
            .gate
            .admit(u64::MAX, self.settings.notify.per_minute, Instant::now())
        {
            tracing::debug!("notification rate-limited");
            return;
        }
        self.deliver(Note::new(
            title,
            body.unwrap_or_default(),
            urgency_byte(urgency),
        ));
    }

    fn deliver(&mut self, note: Note) {
        let app = self.app_id.clone();
        let job = move || notify::deliver(&app, &note);
        if self.workers.submit(Box::new(job)).is_err() {
            tracing::debug!("worker queue full; notification dropped");
        }
    }

    /// `startup` once the first pane runs (T-11 §5).
    pub(super) fn emit_startup(&mut self) {
        let pane = self.focused();
        self.ctl_emit(Event::new(EventKind::Startup, Cause::Config), pane);
    }

    /// Seed persisted consent answers (before the first event).
    pub(super) fn seed_consent(&mut self, always: HashSet<u64>) {
        self.auto.rules.always = always;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_file_round_trips_and_ignores_junk() {
        let set: HashSet<u64> = [1u64, 0xdead_beef_0000_0001].into_iter().collect();
        let text = render_consent(&set);
        assert_eq!(text, "0000000000000001\ndeadbeef00000001\n");
        assert_eq!(parse_consent(&text), set);
        let junk = "zz\n12345\n0000000000000002\n\u{1b}[31m\n00000000000000030\n";
        assert_eq!(
            parse_consent(junk),
            [2u64].into_iter().collect::<HashSet<_>>()
        );
        let many: String = (0..5000u64).map(|i| format!("{i:016x}\n")).collect();
        assert_eq!(parse_consent(&many).len(), CONSENT_MAX);
    }

    #[test]
    fn rule_sheet_ignores_return_and_answers_explicit_keys_after_the_guard() {
        use apoth_input::{Key, Named};
        let spec = rule_bar_spec(apoth_ui::WidgetId(1), "r", "bell", "launch -- htop");
        let guard = apoth_ui::tokens::motion::CONSENT_GUARD_MS;
        let ch = Key::Char;
        for k in ['o', 'a', 'd'] {
            assert_eq!(crate::app::paste_answer(&spec, guard - 1, ch(k)), None);
        }
        assert_eq!(crate::app::paste_answer(&spec, guard, ch('o')), Some(0));
        assert_eq!(crate::app::paste_answer(&spec, guard, ch('a')), Some(1));
        assert_eq!(crate::app::paste_answer(&spec, guard, ch('d')), Some(2));
        assert_eq!(
            crate::app::paste_answer(&spec, guard, Key::Named(Named::Enter)),
            None
        );
        assert_eq!(crate::app::paste_answer(&spec, guard, ch(' ')), None);
    }

    #[test]
    fn urgency_maps_to_freedesktop_bytes() {
        assert_eq!(urgency_byte(None), 1);
        assert_eq!(urgency_byte(Some(Urgency::Low)), 0);
        assert_eq!(urgency_byte(Some(Urgency::Critical)), 2);
    }
}
