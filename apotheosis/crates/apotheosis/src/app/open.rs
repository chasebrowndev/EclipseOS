// SPDX-License-Identifier: AGPL-3.0-only
//! "Open with" on the main loop (T-14 §11): the `open.*` actions, the Open menu and picker
//! on the palette sheet, launching the choice, the `apoth play` fallback and "Remember".
//!
//! The resolve (stat, sniff, shared-mime-info, desktop entries, `mimeapps.list`) and the
//! config write run on a worker; their results come back as [`OpenMsg`]. Paths and URLs are
//! never logged.

use std::path::PathBuf;

use apoth_action::enums::OpenPlacement;
use apoth_config::fs::RealFs;
use apoth_config::model::Layer;
use apoth_config::types::Value;

use super::App;
use crate::nav::PendingOpen;
use crate::open::{
    self, Choice, Handler, Intent, OpenPick, PLAY_UNSUPPORTED, Placement, RawKind, Request,
    Resolved, XdgEnv,
};
use crate::palette::{self as pal, Palette, Pick, Row};
use crate::pane::{MainMsg, PaneKey};
use crate::settings::load_input;
use crate::view::{Copied, CopyTarget, ViewCmd};

/// Worker results for the opener.
#[derive(Debug)]
pub(crate) enum OpenMsg {
    Resolved(PaneKey, Box<Resolved>),
    /// A detached launch with this token exited "unsupported": run its fallback.
    Unsupported(u64),
    /// A "Remember" write finished (`Err` carries a reason, never a path).
    Remembered(Result<(), String>),
}

/// The menu or picker on the sheet.
#[derive(Debug)]
struct Sheet {
    resolved: Resolved,
    choices: Vec<Choice>,
    picker: bool,
    remember: bool,
    pane: PaneKey,
}

#[derive(Debug, Default)]
pub(crate) struct OpenState {
    sheet: Option<Sheet>,
    /// Hint mode started by an `open.*` action with nothing selected.
    hint_intent: Option<(PaneKey, Intent)>,
    /// An `open.default placement=` waiting for the selection answer.
    placement: Option<Placement>,
    /// `apoth play` panes and what runs if they exit "unsupported".
    pane_fallbacks: Vec<(PaneKey, Choice)>,
    /// Detached `apoth play --ext` launches by token.
    detached: Vec<(u64, Choice)>,
    next_token: u64,
}

/// At most this many fallbacks wait at once (older ones are dropped).
const MAX_FALLBACKS: usize = 32;

/// The `apoth` CLI: next to this binary, else on `PATH`.
fn apoth_bin() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("apoth")))
        .filter(|p| p.is_file())
        .map_or_else(|| "apoth".to_string(), |p| p.to_string_lossy().into_owned())
}

/// The argv of a terminal handler (`--ext` for a window; icat holds in a pane).
pub(crate) fn term_argv(
    bin: &str,
    term: open::Term,
    file: &std::path::Path,
    p: Placement,
) -> Vec<String> {
    let mut v = vec![bin.to_string(), term.subcommand().to_string()];
    match (term, p) {
        (_, Placement::Window) => v.push("--ext".into()),
        (open::Term::Icat, _) => v.push("--hold".into()),
        (open::Term::Play, _) => {}
    }
    v.push("--".into());
    v.push(file.to_string_lossy().into_owned());
    v
}

/// The `open.handlers` map with `key` set to `value` (replacing an earlier entry).
pub(crate) fn remembered_map(
    current: &[(String, Vec<String>)],
    key: &str,
    value: Vec<String>,
) -> Value {
    let mut m: Vec<(String, Vec<String>)> = current
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case(key))
        .cloned()
        .collect();
    m.push((key.to_string(), value));
    Value::Map(m)
}

impl App {
    /// `open.default`, `open.with`, `open.menu`.
    pub(super) fn open_action(
        &mut self,
        intent: Intent,
        target: Option<String>,
        placement: Option<OpenPlacement>,
    ) {
        let Some(pane) = self.focused() else { return };
        let placement = placement.map(|p| match p {
            OpenPlacement::Inline => Placement::Inline,
            OpenPlacement::Float => Placement::Float,
            OpenPlacement::Window => Placement::Window,
        });
        match target {
            Some(t) => self.open_request(pane, intent, RawKind::Unknown, t.clone(), t, placement),
            None => {
                self.open.placement = placement;
                self.view_of(pane, ViewCmd::TakeTarget { at: None, intent });
            }
        }
    }

    /// The view answered `TakeTarget`.
    pub(super) fn open_target(
        &mut self,
        pane: PaneKey,
        intent: Intent,
        found: Option<(RawKind, String, String)>,
        at_cell: bool,
    ) {
        let placement = self.open.placement.take();
        match found {
            Some((kind, text, visible)) => {
                self.open_request(pane, intent, kind, text, visible, placement);
            }
            // A right click on nothing does nothing.
            None if at_cell => {}
            // Nothing selected: label what could open.
            None => {
                use crate::hints::Kind;
                self.open.hint_intent = Some((pane, intent));
                self.open.placement = placement;
                self.nav.hints = Some(pane);
                self.view_of(
                    pane,
                    ViewCmd::Hints {
                        kinds: vec![Kind::Link, Kind::Url, Kind::Path],
                        alphabet: self.settings.nav.hint_alphabet.clone(),
                        max: self.settings.nav.hint_max,
                    },
                );
            }
        }
    }

    /// A hint was picked. `true` when the opener took it.
    pub(super) fn open_hint(
        &mut self,
        pane: PaneKey,
        kind: crate::hints::Kind,
        text: &str,
        menu: bool,
    ) -> bool {
        use crate::hints::Kind;
        let raw = match kind {
            Kind::Link => RawKind::Link,
            Kind::Url => RawKind::Url,
            Kind::Path => RawKind::Path,
            _ => return false,
        };
        let asked = self
            .open
            .hint_intent
            .filter(|(p, _)| *p == pane)
            .map(|(_, i)| i);
        let intent = match (menu, asked) {
            (true, _) => Intent::Menu,
            (false, Some(i)) => i,
            // The default action of a URL or link hint opens it; a path hint copies.
            (false, None) if kind.opens() => Intent::Default,
            (false, None) => return false,
        };
        let placement = asked.and(self.open.placement.take());
        // A link hint's text is its URI: the visible text is unknown, so it is shown first.
        let visible = if kind == Kind::Link { "" } else { text };
        self.open_request(
            pane,
            intent,
            raw,
            text.to_string(),
            visible.to_string(),
            placement,
        );
        true
    }

    pub(super) fn open_hints_ended(&mut self, pane: PaneKey) {
        if self.open.hint_intent.is_some_and(|(p, _)| p == pane) {
            self.open.hint_intent = None;
            self.open.placement = None;
        }
    }

    pub(super) fn open_request(
        &mut self,
        pane: PaneKey,
        intent: Intent,
        kind: RawKind,
        raw: String,
        visible: String,
        placement: Option<Placement>,
    ) {
        if raw.is_empty() || raw.len() > apoth_action::action::MAX_OPEN_TARGET_BYTES {
            self.handle.ring_bell(Some(self.win));
            return;
        }
        let c = self.reloader.config();
        let rules = c
            .get("open.handlers")
            .and_then(Value::as_map)
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        let base = c
            .str("open.placement")
            .and_then(Placement::parse)
            .unwrap_or(Placement::Float);
        let opener_custom = c
            .origin("links.open-command")
            .is_some_and(|o| o.layer != Layer::Default);
        let remote = self.open_remote(pane);
        let req = Request {
            intent,
            raw,
            kind,
            visible,
            cwd: self.panes.get(&pane).and_then(|p| p.cwd.clone()),
            remote,
            hostname: self.hostname.clone(),
            schemes: self.settings.nav.schemes.clone(),
            rules,
            placement: placement.unwrap_or(base),
            forced_placement: placement.is_some(),
            opener: self.settings.nav.open_command.clone(),
            opener_custom,
            env: XdgEnv::from(&|k| std::env::var(k).ok()),
        };
        let tx = self.to_main.clone();
        let job = move || {
            let r = open::resolve(&req);
            let _ = tx.send(MainMsg::Open(OpenMsg::Resolved(pane, Box::new(r))));
        };
        if self.workers.submit(Box::new(job)).is_err() {
            tracing::warn!("worker queue full: open dropped");
            self.handle.ring_bell(Some(self.win));
        }
    }

    /// `Some(host label)` when the pane is remote: its foreground is a remote session, or
    /// its OSC 7 directory names another host (T-18 §10).
    fn open_remote(&self, pane: PaneKey) -> Option<String> {
        let host = self.panes.get(&pane).and_then(|p| p.cwd_host.clone());
        if let Some(h) = host {
            return Some(open::clean_host(&h));
        }
        self.pane_remote(pane).then(|| "a remote host".to_string())
    }

    pub(super) fn on_open(&mut self, m: OpenMsg) {
        match m {
            OpenMsg::Resolved(pane, r) => self.open_resolved(pane, *r),
            OpenMsg::Unsupported(token) => {
                if let Some(i) = self.open.detached.iter().position(|d| d.0 == token) {
                    let (_, c) = self.open.detached.swap_remove(i);
                    tracing::info!("apoth play: unsupported, opening with the desktop default");
                    self.open_launch(c, None, None);
                }
            }
            OpenMsg::Remembered(Ok(())) => self.start_reload(),
            OpenMsg::Remembered(Err(why)) => {
                tracing::warn!(reason = %why, "open: remember not written");
                self.handle.ring_bell(Some(self.win));
            }
        }
    }

    fn open_resolved(&mut self, pane: PaneKey, r: Resolved) {
        match r.intent {
            Intent::Default => {
                if let Some(why) = &r.refusal {
                    tracing::info!(reason = %why, "open refused");
                    self.handle.ring_bell(Some(self.win));
                    return;
                }
                let Some(choice) = r.default.clone() else {
                    tracing::info!("open: no handler");
                    self.handle.ring_bell(Some(self.win));
                    return;
                };
                if r.reveal {
                    // T-14 §5: the full URI is confirmed before a link opens.
                    self.nav.open = Some(PendingOpen {
                        uri: r.shown.clone(),
                        choice: Some(Box::new((choice, r.fallback.clone(), pane))),
                    });
                    self.bar_dismissed = false;
                    self.refresh_bar();
                    return;
                }
                self.open_launch(choice, r.fallback.clone(), Some(pane));
            }
            Intent::Menu => self.open_sheet(pane, r, false),
            Intent::Picker => self.open_sheet(pane, r, true),
        }
    }

    fn open_sheet(&mut self, pane: PaneKey, resolved: Resolved, picker: bool) {
        let mut p = Palette::new(pal::Mode::Open, "");
        let to_row = |r: open::MenuRow| Row {
            title: r.title,
            hint: r.hint,
            pick: Pick::Open(r.pick),
            program: r.program,
        };
        let choices = if picker {
            let (pinned, rows, choices) = open::picker_rows(&resolved, false);
            p.pinned = pinned.into_iter().map(to_row).collect();
            p.source = rows.into_iter().map(to_row).collect();
            choices
        } else {
            let (rows, choices) = open::menu_rows(&resolved);
            p.source = rows.into_iter().map(to_row).collect();
            choices
        };
        p.refresh(&self.keymap, &[]);
        self.palette = Some(p);
        self.open.sheet = Some(Sheet {
            resolved,
            choices,
            picker,
            remember: false,
            pane,
        });
        self.ui_state = apoth_ui::UiState::default();
        self.send_layout();
    }

    /// The sheet's placeholder: the target, as program text.
    pub(super) fn open_sheet_title(&self) -> Option<String> {
        let s = self.open.sheet.as_ref()?;
        let prefix = if s.picker { "Open with: " } else { "" };
        Some(format!("{prefix}{}", s.resolved.shown))
    }

    /// The palette closed: the sheet goes with it.
    pub(super) fn open_sheet_closed(&mut self) {
        self.open.sheet = None;
    }

    pub(super) fn open_pick(&mut self, pick: OpenPick) {
        match pick {
            OpenPick::Launch(i) => {
                let Some(s) = self.open.sheet.as_ref() else {
                    return;
                };
                let Some(c) = s.choices.get(i).cloned() else {
                    return;
                };
                let fallback = (Some(&c) == s.resolved.default.as_ref())
                    .then(|| s.resolved.fallback.clone())
                    .flatten();
                let pane = s.pane;
                if s.picker
                    && s.remember
                    && let Some(key) = s.resolved.remember_key()
                {
                    self.open_remember(key.to_string(), c.rule_value());
                }
                self.close_palette();
                self.open_launch(c, fallback, Some(pane));
            }
            OpenPick::Picker => {
                self.close_palette();
                if let Some(s) = self.open.sheet.take() {
                    self.open_sheet(s.pane, s.resolved, true);
                }
            }
            OpenPick::Copy(text) => {
                self.close_palette();
                self.on_copied(Copied {
                    target: CopyTarget::Clipboard,
                    text,
                });
            }
            OpenPick::ToggleRemember => {
                let Some(s) = self.open.sheet.as_mut() else {
                    return;
                };
                s.remember = !s.remember;
                let (pinned, _, _) = open::picker_rows(&s.resolved, s.remember);
                if let Some(p) = self.palette.as_mut() {
                    let selected = p.selected;
                    p.pinned = pinned
                        .into_iter()
                        .map(|r| Row {
                            title: r.title,
                            hint: r.hint,
                            pick: Pick::Open(r.pick),
                            program: r.program,
                        })
                        .collect();
                    p.refresh(&self.keymap, &[]);
                    p.selected = selected.min(p.rows.len().saturating_sub(1));
                }
                self.send_layout();
            }
            OpenPick::Disabled => {
                let _ = self.handle.ring_bell(Some(self.win));
            }
        }
    }

    /// The reveal bar was accepted for a resolved link.
    pub(super) fn open_confirmed(&mut self, c: (Choice, Option<Choice>, PaneKey)) {
        let (choice, fallback, pane) = c;
        self.open_launch(choice, fallback, Some(pane));
    }

    /// Launch a choice: terminal handlers and `Terminal=true` apps in a pane, the rest
    /// detached. Always an argv, never a shell.
    fn open_launch(&mut self, c: Choice, fallback: Option<Choice>, pane: Option<PaneKey>) {
        if let Some(p) = pane
            && self.panes.contains_key(&p)
        {
            // New panes go next to the pane the target came from.
            self.focus_pane_at(p);
        }
        match c.handler {
            Handler::Term { term, file } => {
                let argv = term_argv(&apoth_bin(), term, &file, c.placement);
                let fb = (term == open::Term::Play).then_some(fallback).flatten();
                match c.placement {
                    Placement::Window => {
                        let token = fb.map(|f| self.open_track_detached(f));
                        self.open_detached(argv, token);
                    }
                    p => {
                        let id = self.open_in_pane(argv, p);
                        if let (Some(id), Some(f)) = (id, fb) {
                            if self.open.pane_fallbacks.len() >= MAX_FALLBACKS {
                                self.open.pane_fallbacks.remove(0);
                            }
                            self.open.pane_fallbacks.push((id, f));
                        }
                    }
                }
            }
            Handler::Desktop {
                argv,
                terminal: true,
                ..
            } => {
                let p = match c.placement {
                    Placement::Window => Placement::Float,
                    p => p,
                };
                self.open_in_pane(argv, p);
            }
            Handler::Desktop { argv, .. } | Handler::Opener { argv } => {
                self.open_detached(argv, None);
            }
        }
    }

    fn open_track_detached(&mut self, f: Choice) -> u64 {
        self.open.next_token += 1;
        if self.open.detached.len() >= MAX_FALLBACKS {
            self.open.detached.remove(0);
        }
        self.open.detached.push((self.open.next_token, f));
        self.open.next_token
    }

    fn open_in_pane(&mut self, argv: Vec<String>, p: Placement) -> Option<PaneKey> {
        match p {
            Placement::Inline => self.split(apoth_mux::Dir::Down, Some(argv), None, Vec::new()),
            _ => self.float(Some(argv), None, Vec::new()),
        }
    }

    /// Spawn on a worker with a reaper thread (the pool is never held by a long-lived app).
    /// With a token, an "unsupported" exit comes back as [`OpenMsg::Unsupported`].
    fn open_detached(&mut self, argv: Vec<String>, token: Option<u64>) {
        let socket: Option<PathBuf> = self.ctl_path().map(std::path::Path::to_path_buf);
        let tx = self.to_main.clone();
        let job = move || {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .env_remove("XDG_ACTIVATION_TOKEN")
                .env_remove("DESKTOP_STARTUP_ID");
            if let Some(s) = &socket {
                c.env("APOTHEOSIS_SOCKET", s);
            }
            match c.spawn() {
                Ok(mut child) => {
                    let _ = std::thread::Builder::new()
                        .name("apoth-open-reap".into())
                        .spawn(move || {
                            let code = child.wait().ok().and_then(|s| s.code());
                            if let (Some(t), Some(code)) = (token, code)
                                && PLAY_UNSUPPORTED.contains(&code)
                            {
                                let _ = tx.send(MainMsg::Open(OpenMsg::Unsupported(t)));
                            }
                        });
                }
                Err(e) => tracing::warn!(error = %e.kind(), "open: app did not start"),
            }
        };
        if self.workers.submit(Box::new(job)).is_err() {
            tracing::warn!("worker queue full: open dropped");
        }
    }

    /// A pane went: an `apoth play` that could not play hands over to its fallback.
    pub(super) fn open_pane_gone(&mut self, id: PaneKey, code: Option<i32>) {
        let Some(i) = self.open.pane_fallbacks.iter().position(|f| f.0 == id) else {
            return;
        };
        let (_, f) = self.open.pane_fallbacks.swap_remove(i);
        if code.is_some_and(|c| PLAY_UNSUPPORTED.contains(&c)) {
            tracing::info!("apoth play: unsupported, opening with the desktop default");
            self.open_launch(f, None, None);
        }
    }

    /// "Remember for <type>": write `open.handlers` in the user config on a worker, then
    /// reload.
    fn open_remember(&mut self, key: String, value: Vec<String>) {
        let cfg = self.reloader.config().clone();
        let launch = self.launch.clone();
        let tx = self.to_main.clone();
        let job = move || {
            let r = remember_write(&RealFs, &cfg, &load_input(&launch), &key, value);
            let _ = tx.send(MainMsg::Open(OpenMsg::Remembered(r)));
        };
        if self.workers.submit(Box::new(job)).is_err() {
            tracing::warn!("worker queue full: remember dropped");
        }
    }
}

/// Write the rule into the user's main config file (or the file that already sets
/// `open.handlers`).
pub(crate) fn remember_write(
    fs: &dyn apoth_config::fs::Fs,
    cfg: &apoth_config::Config,
    input: &apoth_config::LoadInput,
    key: &str,
    value: Vec<String>,
) -> Result<(), String> {
    apoth_config::lang::validate_open_handler(key, &value)?;
    let Some(user) = input.user.as_deref() else {
        return Err("no user config file".into());
    };
    let current = cfg
        .get("open.handlers")
        .and_then(Value::as_map)
        .unwrap_or(&[]);
    let v = remembered_map(current, key, value);
    apoth_config::edit::apply_write(fs, cfg, user, &[], "open.handlers", Some(&v), None)
        .map(|_| ())
        .map_err(|e| match e {
            apoth_config::EditError::Io(k) => format!("io: {k}"),
            _ => "edit refused".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn terminal_handler_argv() {
        let f = Path::new("/tmp/a b.png");
        assert_eq!(
            term_argv("apoth", open::Term::Icat, f, Placement::Float),
            vec!["apoth", "icat", "--hold", "--", "/tmp/a b.png"]
        );
        assert_eq!(
            term_argv("/usr/bin/apoth", open::Term::Icat, f, Placement::Window),
            vec!["/usr/bin/apoth", "icat", "--ext", "--", "/tmp/a b.png"]
        );
        assert_eq!(
            term_argv("apoth", open::Term::Play, f, Placement::Inline),
            vec!["apoth", "play", "--", "/tmp/a b.png"]
        );
    }

    #[test]
    fn remembered_map_replaces_the_key() {
        let cur = vec![
            ("image/png".to_string(), vec!["gimp.desktop".to_string()]),
            ("text/*".to_string(), vec!["opener".to_string()]),
        ];
        let Value::Map(m) = remembered_map(
            &cur,
            "image/png",
            vec!["apoth-icat".into(), "window".into()],
        ) else {
            panic!()
        };
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].0, "text/*");
        assert_eq!(
            m[1],
            (
                "image/png".into(),
                vec!["apoth-icat".into(), "window".into()]
            )
        );
    }

    #[test]
    fn remember_writes_the_user_config() {
        let dir = std::env::temp_dir().join(format!("apoth-remember-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("apotheosis.kdl");
        std::fs::write(&user, "// mine\nfont { size 12 }\n").unwrap();
        let input = apoth_config::LoadInput {
            user: Some(user.clone()),
            ..Default::default()
        };
        let cfg = apoth_config::load(&RealFs, &input, None).config;
        remember_write(
            &RealFs,
            &cfg,
            &input,
            "image/png",
            vec!["apoth-icat".into(), "window".into()],
        )
        .unwrap();
        let text = std::fs::read_to_string(&user).unwrap();
        assert!(text.contains("// mine"), "{text}");
        let cfg = apoth_config::load(&RealFs, &input, None).config;
        assert_eq!(
            cfg.get("open.handlers").and_then(Value::as_map),
            Some(
                &[(
                    "image/png".to_string(),
                    vec!["apoth-icat".to_string(), "window".to_string()]
                )][..]
            )
        );
        // A bad rule is refused before anything is written.
        assert!(
            remember_write(&RealFs, &cfg, &input, "image/png", vec!["sh -c x".into()]).is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
