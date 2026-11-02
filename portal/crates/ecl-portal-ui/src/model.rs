// SPDX-License-Identifier: AGPL-3.0-only
//! The panes' state, with no iced in it: selection, keyboard and the mapping
//! from a choice to a `PaneReply`. Everything a pane can do is a method here,
//! so the views are only a picture of it and the tests need no window.
//!
//! A pane only ever makes a *choice*. The daemon turns it into a request to
//! the compositor, which asks the human again (ADR 0085); nothing here grants.

use ecl_portal_proto::{PaneReply, PaneRequest, Requester, ScreenshotMode, Source};

use crate::metrics;

/// What a key press means to a pane, after the keyboard layer has read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
    /// Tab: the next tab, or the next control.
    Next,
    /// Shift+Tab.
    Prev,
    Space,
    Enter,
    Escape,
    /// `a`: the audio switch.
    Audio,
    /// `r`: the remember switch.
    Remember,
}

/// Requester-supplied text, made safe to lay out: control characters and
/// newlines become spaces, runs of spaces collapse, and the result is cut to
/// `max` characters with an ellipsis. Every string the requester controls
/// goes through this, so a hostile name cannot push the real controls off the
/// sheet or fake a second paragraph.
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut space = true;
    for c in s.chars() {
        // Bidi overrides would let a name draw as something else.
        if matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
        {
            continue;
        }
        let c = if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
            ' '
        } else {
            c
        };
        if c == ' ' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    let out = out.trim_end();
    if out.chars().count() > max {
        let mut cut: String = out.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    } else {
        out.to_owned()
    }
}

// ---- source chooser --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Screen,
    Window,
}

impl Tab {
    pub fn of(s: &Source) -> Tab {
        match s {
            Source::Output { .. } => Tab::Screen,
            Source::Window { .. } => Tab::Window,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Chooser {
    sources: Vec<Source>,
    tabs: Vec<Tab>,
    tab: Tab,
    /// Indices into `sources`, in the order they were picked.
    selected: Vec<usize>,
    /// Keyboard position, within the current tab's visible list.
    cursor: usize,
    pub audio: bool,
    pub remember: bool,
    allow_multiple: bool,
    allow_audio: bool,
}

impl Chooser {
    pub fn new(
        sources: Vec<Source>,
        allow_multiple: bool,
        allow_audio: bool,
        allow_window: bool,
        allow_monitor: bool,
        remembered: Option<&Source>,
    ) -> Self {
        // A request that allows neither is a daemon bug; showing both is the
        // only answer that leaves the human something to choose.
        let (allow_window, allow_monitor) = if allow_window || allow_monitor {
            (allow_window, allow_monitor)
        } else {
            (true, true)
        };
        let mut tabs = Vec::new();
        if allow_monitor {
            tabs.push(Tab::Screen);
        }
        if allow_window {
            tabs.push(Tab::Window);
        }
        let sources: Vec<Source> = sources
            .into_iter()
            .filter(|s| tabs.contains(&Tab::of(s)))
            .collect();
        let mut me = Self {
            tab: tabs[0],
            tabs,
            sources,
            selected: Vec::new(),
            cursor: 0,
            audio: false,
            remember: false,
            allow_multiple,
            allow_audio,
        };
        // The remembered choice is offered, selected, never taken: the human
        // still presses Share.
        if let Some(i) = remembered.and_then(|r| me.sources.iter().position(|s| s == r)) {
            me.selected.push(i);
            me.tab = Tab::of(&me.sources[i]);
        }
        me.cursor = me.first_cursor();
        me
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn allow_audio(&self) -> bool {
        self.allow_audio
    }

    pub fn source(&self, i: usize) -> &Source {
        &self.sources[i]
    }

    /// Indices of the sources the current tab shows.
    pub fn visible(&self) -> Vec<usize> {
        self.sources
            .iter()
            .enumerate()
            .filter(|(_, s)| Tab::of(s) == self.tab)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn is_selected(&self, i: usize) -> bool {
        self.selected.contains(&i)
    }

    /// The tile the keyboard is on, as an index into the sources.
    pub fn cursor_source(&self) -> Option<usize> {
        self.visible().get(self.cursor).copied()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn can_share(&self) -> bool {
        !self.selected.is_empty()
    }

    fn first_cursor(&self) -> usize {
        let vis = self.visible();
        vis.iter().position(|i| self.is_selected(*i)).unwrap_or(0)
    }

    pub fn set_tab(&mut self, tab: Tab) {
        if !self.tabs.contains(&tab) || tab == self.tab {
            return;
        }
        self.tab = tab;
        // With one choice allowed, a pick on the tab you left would be shared
        // unseen: leaving the tab drops it.
        if !self.allow_multiple {
            self.selected.clear();
        }
        self.cursor = self.first_cursor();
    }

    pub fn cycle_tab(&mut self, back: bool) {
        let n = self.tabs.len();
        let at = self.tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
        let to = if back { (at + n - 1) % n } else { (at + 1) % n };
        self.set_tab(self.tabs[to]);
    }

    /// Select (or deselect) source `i`. With one choice allowed, selecting
    /// replaces; picking the selected tile again clears it.
    pub fn toggle(&mut self, i: usize) {
        if i >= self.sources.len() {
            return;
        }
        if let Some(at) = self.selected.iter().position(|s| *s == i) {
            self.selected.remove(at);
        } else {
            if !self.allow_multiple {
                self.selected.clear();
            }
            self.selected.push(i);
        }
        if let Some(c) = self.visible().iter().position(|v| *v == i) {
            self.cursor = c;
        }
    }

    pub fn set_audio(&mut self, on: bool) {
        self.audio = on && self.allow_audio;
    }

    pub fn move_cursor(&mut self, nav: Nav) {
        let len = self.visible().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let cols = metrics::GRID_COLS;
        let c = self.cursor.min(len - 1);
        self.cursor = match nav {
            Nav::Left => c.saturating_sub(1),
            Nav::Right => (c + 1).min(len - 1),
            Nav::Up if c >= cols => c - cols,
            Nav::Down if c + cols < len => c + cols,
            // A short last row: Down from above it lands on its last tile.
            Nav::Down if c / cols < (len - 1) / cols => len - 1,
            _ => c,
        };
    }

    pub fn reply(&self) -> PaneReply {
        PaneReply::Sources {
            chosen: self
                .selected
                .iter()
                .map(|i| self.sources[*i].clone())
                .collect(),
            audio: self.audio && self.allow_audio,
            remember: self.remember,
        }
    }

    /// Enter selects the tile under the cursor when nothing is picked yet, and
    /// shares what is picked otherwise: so the keyboard path is
    /// arrows, Enter, Enter, and Share is never one stray key.
    pub fn key(&mut self, nav: Nav) -> Option<PaneReply> {
        match nav {
            Nav::Escape => return Some(PaneReply::Cancelled),
            Nav::Left | Nav::Right | Nav::Up | Nav::Down => self.move_cursor(nav),
            Nav::Next => self.cycle_tab(false),
            Nav::Prev => self.cycle_tab(true),
            Nav::Space => {
                if let Some(i) = self.cursor_source() {
                    self.toggle(i);
                }
            }
            Nav::Enter => {
                if self.can_share() {
                    return Some(self.reply());
                }
                if let Some(i) = self.cursor_source() {
                    self.toggle(i);
                }
            }
            Nav::Audio => self.set_audio(!self.audio),
            Nav::Remember => self.remember = !self.remember,
        }
        None
    }
}

// ---- yes / no sheets -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Deny,
    Allow,
}

/// The state of a two-button question. Deny holds the focus when it opens,
/// and Enter activates whatever holds it: Enter alone never allows.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub focus: Choice,
    pub remember: bool,
    allow_remember: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Stay,
    Decide { allow: bool, remember: bool },
    Cancel,
}

impl Confirm {
    pub fn new(allow_remember: bool) -> Self {
        Self {
            focus: Choice::Deny,
            remember: false,
            allow_remember,
        }
    }

    pub fn allow_remember(&self) -> bool {
        self.allow_remember
    }

    pub fn set_remember(&mut self, on: bool) {
        self.remember = on && self.allow_remember;
    }

    pub fn decide(&self, choice: Choice) -> Verdict {
        Verdict::Decide {
            allow: choice == Choice::Allow,
            remember: self.remember && self.allow_remember,
        }
    }

    pub fn key(&mut self, nav: Nav) -> Verdict {
        match nav {
            Nav::Escape => return Verdict::Cancel,
            Nav::Left => self.focus = Choice::Deny,
            Nav::Right => self.focus = Choice::Allow,
            Nav::Next | Nav::Prev => {
                self.focus = match self.focus {
                    Choice::Deny => Choice::Allow,
                    Choice::Allow => Choice::Deny,
                }
            }
            Nav::Remember => self.set_remember(!self.remember),
            Nav::Enter => return self.decide(self.focus),
            _ => {}
        }
        Verdict::Stay
    }
}

// ---- screenshot mode -------------------------------------------------------

pub const MODES: [ScreenshotMode; 3] = [
    ScreenshotMode::Screen,
    ScreenshotMode::Window,
    ScreenshotMode::Area,
];

#[derive(Debug, Clone)]
pub struct ShotPicker {
    pub mode: ScreenshotMode,
}

impl ShotPicker {
    pub fn new() -> Self {
        Self { mode: MODES[0] }
    }

    fn at(&self) -> usize {
        MODES.iter().position(|m| *m == self.mode).unwrap_or(0)
    }

    pub fn key(&mut self, nav: Nav) -> Option<PaneReply> {
        let n = MODES.len();
        match nav {
            Nav::Escape => return Some(PaneReply::Cancelled),
            Nav::Left => self.mode = MODES[self.at().saturating_sub(1)],
            Nav::Right => self.mode = MODES[(self.at() + 1).min(n - 1)],
            Nav::Next => self.mode = MODES[(self.at() + 1) % n],
            Nav::Prev => self.mode = MODES[(self.at() + n - 1) % n],
            Nav::Enter => return Some(PaneReply::ScreenshotMode(self.mode)),
            _ => {}
        }
        None
    }
}

impl Default for ShotPicker {
    fn default() -> Self {
        Self::new()
    }
}

// ---- colour ----------------------------------------------------------------

/// `#rrggbb` or `rrggbb`. Anything else is `None`.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

#[derive(Debug, Clone)]
pub struct ColorPick {
    rgba: [u8; 4],
    /// What the hex field shows: the typed draft while it is being typed.
    pub draft: String,
}

impl ColorPick {
    pub fn new(start: [u8; 3]) -> Self {
        let mut me = Self {
            rgba: [start[0], start[1], start[2], u8::MAX],
            draft: String::new(),
        };
        me.draft = me.hex();
        me
    }

    pub fn rgba(&self) -> [u8; 4] {
        self.rgba
    }

    pub fn hex(&self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            self.rgba[0], self.rgba[1], self.rgba[2]
        )
    }

    /// The picker moved: the field follows.
    pub fn set_rgba(&mut self, rgba: [u8; 4]) {
        self.rgba = [rgba[0], rgba[1], rgba[2], u8::MAX];
        self.draft = self.hex();
    }

    /// The field was typed in: the colour follows once it parses.
    pub fn set_draft(&mut self, draft: String) {
        if let Some([r, g, b]) = parse_hex(&draft) {
            self.rgba = [r, g, b, u8::MAX];
        }
        self.draft = draft;
    }

    pub fn draft_valid(&self) -> bool {
        parse_hex(&self.draft).is_some()
    }

    pub fn reply(&self) -> PaneReply {
        let f = |v: u8| f64::from(v) / 255.0;
        PaneReply::Color {
            r: f(self.rgba[0]),
            g: f(self.rgba[1]),
            b: f(self.rgba[2]),
        }
    }

    pub fn key(&mut self, nav: Nav) -> Option<PaneReply> {
        match nav {
            Nav::Escape => Some(PaneReply::Cancelled),
            // A half-typed field is not a colour the human chose.
            Nav::Enter if self.draft_valid() => Some(self.reply()),
            _ => None,
        }
    }
}

// ---- account ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AccountForm {
    pub name: String,
    pub real_name: String,
    pub image: Option<String>,
}

/// The login name and display name for `uid` out of `/etc/passwd` text.
pub fn passwd_entry(passwd: &str, uid: u32) -> Option<(String, String)> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 5 || f[2].parse::<u32>().ok()? != uid {
            return None;
        }
        let real = f[4].split(',').next().unwrap_or_default().trim();
        let shown = if real.is_empty() { f[0] } else { real };
        Some((f[0].to_owned(), shown.to_owned()))
    })
}

impl AccountForm {
    pub fn new(name: String, real_name: String, image: Option<String>) -> Self {
        Self {
            name,
            real_name,
            image,
        }
    }

    pub fn can_share(&self) -> bool {
        !self.name.trim().is_empty()
    }

    pub fn reply(&self) -> PaneReply {
        PaneReply::Account {
            name: self.name.trim().to_owned(),
            real_name: self.real_name.trim().to_owned(),
            image: self.image.clone(),
        }
    }

    pub fn key(&mut self, nav: Nav) -> Option<PaneReply> {
        match nav {
            Nav::Escape => Some(PaneReply::Cancelled),
            Nav::Enter if self.can_share() => Some(self.reply()),
            _ => None,
        }
    }
}

// ---- the whole pane --------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Pane {
    Source(Chooser),
    Permission {
        interface: String,
        title: String,
        detail: String,
        confirm: Confirm,
    },
    Shot(ShotPicker),
    Color(ColorPick),
    Account {
        reason: String,
        form: AccountForm,
    },
    Email {
        addresses: Vec<String>,
        confirm: Confirm,
    },
}

/// Where a dialog's window and content differ by request.
pub struct Opened {
    pub requester: Requester,
    pub pane: Pane,
}

impl Pane {
    /// `identity`: the login, real name and photo, read by the caller (the
    /// only part that touches the system).
    pub fn open(
        req: PaneRequest,
        identity: (String, String, Option<String>),
        color_start: [u8; 3],
    ) -> Opened {
        match req {
            PaneRequest::PickSource {
                requester,
                sources,
                allow_multiple,
                allow_audio,
                allow_window,
                allow_monitor,
                remembered,
            } => Opened {
                requester,
                pane: Pane::Source(Chooser::new(
                    sources,
                    allow_multiple,
                    allow_audio,
                    allow_window,
                    allow_monitor,
                    remembered.as_ref(),
                )),
            },
            PaneRequest::Permission {
                requester,
                interface,
                title,
                detail,
                allow_remember,
            } => Opened {
                requester,
                pane: Pane::Permission {
                    interface: clean(&interface, metrics::MAX_TITLE),
                    title: clean(&title, metrics::MAX_TITLE),
                    detail: clean(&detail, metrics::MAX_DETAIL),
                    confirm: Confirm::new(allow_remember),
                },
            },
            PaneRequest::ScreenshotMode { requester } => Opened {
                requester,
                pane: Pane::Shot(ShotPicker::new()),
            },
            PaneRequest::PickColor { requester } => Opened {
                requester,
                pane: Pane::Color(ColorPick::new(color_start)),
            },
            PaneRequest::Account { requester, reason } => Opened {
                requester,
                pane: Pane::Account {
                    reason: clean(&reason, metrics::MAX_DETAIL),
                    form: AccountForm::new(identity.0, identity.1, identity.2),
                },
            },
            PaneRequest::Email {
                requester,
                addresses,
            } => Opened {
                requester,
                pane: Pane::Email {
                    addresses: addresses
                        .iter()
                        .map(|a| clean(a, metrics::MAX_ADDRESS))
                        .collect(),
                    confirm: Confirm::new(false),
                },
            },
        }
    }

    /// A key press; `Some` when it ends the dialog.
    pub fn key(&mut self, nav: Nav) -> Option<PaneReply> {
        match self {
            Pane::Source(c) => c.key(nav),
            Pane::Shot(s) => s.key(nav),
            Pane::Color(c) => c.key(nav),
            Pane::Account { form, .. } => form.key(nav),
            Pane::Permission { confirm, .. } => match confirm.key(nav) {
                Verdict::Stay => None,
                Verdict::Cancel => Some(PaneReply::Cancelled),
                Verdict::Decide { allow, remember } => {
                    Some(PaneReply::Permission { allow, remember })
                }
            },
            Pane::Email { confirm, .. } => match confirm.key(nav) {
                Verdict::Stay => None,
                Verdict::Cancel => Some(PaneReply::Cancelled),
                Verdict::Decide { allow, .. } => Some(PaneReply::Email { send: allow }),
            },
        }
    }

    /// Window size and title for this pane.
    pub fn window(&self) -> (f32, f32, &'static str) {
        match self {
            Pane::Source(_) => (metrics::SOURCE_W, metrics::SOURCE_H, "Choose what to share"),
            Pane::Permission { .. } => (metrics::SHEET_W, metrics::SHEET_H, "Permission request"),
            Pane::Shot(_) => (metrics::SHOT_W, metrics::SHOT_H, "Take a screenshot"),
            Pane::Color(_) => (metrics::COLOR_W, metrics::COLOR_H, "Pick a colour"),
            Pane::Account { .. } => (
                metrics::ACCOUNT_W,
                metrics::ACCOUNT_H,
                "Share account details",
            ),
            Pane::Email { .. } => (metrics::SHEET_W, metrics::SHEET_H, "Open your mail app"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecl_portal_proto::IdentitySource;

    fn out(name: &str) -> Source {
        Source::Output {
            name: name.into(),
            width: 2560,
            height: 1440,
        }
    }

    fn win(id: u64) -> Source {
        Source::Window {
            id,
            title: format!("w{id}"),
            app_id: "app".into(),
        }
    }

    fn mixed() -> Vec<Source> {
        vec![out("DP-1"), out("HDMI-A-1"), win(1), win(2), win(3)]
    }

    fn chooser(multi: bool) -> Chooser {
        Chooser::new(mixed(), multi, true, true, true, None)
    }

    fn chosen(r: &PaneReply) -> Vec<Source> {
        match r {
            PaneReply::Sources { chosen, .. } => chosen.clone(),
            other => panic!("not a source reply: {other:?}"),
        }
    }

    #[test]
    fn opens_on_screens_with_nothing_picked() {
        let c = chooser(false);
        assert_eq!(c.tab(), Tab::Screen);
        assert_eq!(c.visible(), vec![0, 1]);
        assert!(!c.can_share());
    }

    #[test]
    fn the_tab_set_follows_what_is_allowed() {
        let w = Chooser::new(mixed(), false, false, true, false, None);
        assert_eq!(w.tabs(), &[Tab::Window]);
        assert_eq!(w.visible(), vec![0, 1, 2]);
        let neither = Chooser::new(mixed(), false, false, false, false, None);
        assert_eq!(neither.tabs(), &[Tab::Screen, Tab::Window]);
    }

    #[test]
    fn a_remembered_choice_is_selected_on_its_tab_but_not_shared() {
        let c = Chooser::new(mixed(), false, true, true, true, Some(&win(2)));
        assert_eq!(c.tab(), Tab::Window);
        assert!(c.is_selected(3));
        assert_eq!(c.cursor_source(), Some(3));
        // Offered, not taken: the human still presses Share.
        let gone = Chooser::new(mixed(), false, true, true, true, Some(&win(99)));
        assert!(!gone.can_share());
    }

    #[test]
    fn single_select_replaces_and_a_second_press_clears() {
        let mut c = chooser(false);
        c.toggle(0);
        c.toggle(1);
        assert_eq!(chosen(&c.reply()), vec![out("HDMI-A-1")]);
        c.toggle(1);
        assert!(!c.can_share());
    }

    #[test]
    fn multi_select_keeps_pick_order() {
        let mut c = chooser(true);
        c.toggle(1);
        c.toggle(0);
        assert_eq!(chosen(&c.reply()), vec![out("HDMI-A-1"), out("DP-1")]);
        c.toggle(1);
        assert_eq!(chosen(&c.reply()), vec![out("DP-1")]);
    }

    #[test]
    fn leaving_the_tab_drops_a_single_pick_but_not_a_multi_pick() {
        let mut one = chooser(false);
        one.toggle(0);
        one.set_tab(Tab::Window);
        assert!(!one.can_share());
        let mut many = chooser(true);
        many.toggle(0);
        many.set_tab(Tab::Window);
        assert_eq!(many.selected_count(), 1);
    }

    #[test]
    fn tab_key_cycles_both_ways() {
        let mut c = chooser(false);
        c.key(Nav::Next);
        assert_eq!(c.tab(), Tab::Window);
        c.key(Nav::Next);
        assert_eq!(c.tab(), Tab::Screen);
        c.key(Nav::Prev);
        assert_eq!(c.tab(), Tab::Window);
    }

    #[test]
    fn grid_navigation_clamps_and_lands_on_a_short_row() {
        let mut c = Chooser::new((0..5).map(win).collect(), false, false, true, false, None);
        // 3 columns: row 0 is 0..3, row 1 is 3..5.
        c.key(Nav::Right);
        c.key(Nav::Right);
        assert_eq!(c.cursor_source(), Some(2));
        c.key(Nav::Right);
        assert_eq!(c.cursor_source(), Some(3));
        c.key(Nav::Up);
        assert_eq!(c.cursor_source(), Some(0));
        c.key(Nav::Up);
        assert_eq!(c.cursor_source(), Some(0));
        c.key(Nav::Right);
        c.key(Nav::Right);
        c.key(Nav::Down);
        // Below column 2 there is no tile: the short row's last one.
        assert_eq!(c.cursor_source(), Some(4));
        c.key(Nav::Down);
        assert_eq!(c.cursor_source(), Some(4));
        c.key(Nav::Left);
        c.key(Nav::Left);
        c.key(Nav::Left);
        c.key(Nav::Left);
        c.key(Nav::Left);
        assert_eq!(c.cursor_source(), Some(0));
    }

    #[test]
    fn enter_selects_then_shares_and_never_shares_nothing() {
        let mut c = chooser(false);
        assert_eq!(c.key(Nav::Enter), None);
        assert!(c.is_selected(0));
        let r = c.key(Nav::Enter).expect("second enter shares");
        assert_eq!(chosen(&r), vec![out("DP-1")]);
    }

    #[test]
    fn space_toggles_the_tile_under_the_cursor() {
        let mut c = chooser(true);
        c.key(Nav::Right);
        c.key(Nav::Space);
        c.key(Nav::Left);
        c.key(Nav::Space);
        assert_eq!(c.selected_count(), 2);
        c.key(Nav::Space);
        assert_eq!(c.selected_count(), 1);
    }

    #[test]
    fn escape_cancels_everywhere() {
        assert_eq!(chooser(false).key(Nav::Escape), Some(PaneReply::Cancelled));
        assert_eq!(Confirm::new(true).key(Nav::Escape), Verdict::Cancel);
        assert_eq!(
            ShotPicker::new().key(Nav::Escape),
            Some(PaneReply::Cancelled)
        );
        assert_eq!(
            ColorPick::new([1, 2, 3]).key(Nav::Escape),
            Some(PaneReply::Cancelled)
        );
        assert_eq!(
            AccountForm::new("a".into(), "A".into(), None).key(Nav::Escape),
            Some(PaneReply::Cancelled)
        );
    }

    #[test]
    fn audio_and_remember_map_into_the_reply() {
        let mut c = chooser(false);
        c.toggle(0);
        c.key(Nav::Audio);
        c.key(Nav::Remember);
        match c.reply() {
            PaneReply::Sources {
                audio, remember, ..
            } => assert!(audio && remember),
            other => panic!("{other:?}"),
        }
        // A request that did not offer audio cannot carry audio out.
        let mut quiet = Chooser::new(mixed(), false, false, true, true, None);
        quiet.toggle(0);
        quiet.set_audio(true);
        match quiet.reply() {
            PaneReply::Sources { audio, .. } => assert!(!audio),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn deny_holds_focus_and_enter_alone_never_allows() {
        let mut c = Confirm::new(true);
        assert_eq!(c.focus, Choice::Deny);
        assert_eq!(
            c.key(Nav::Enter),
            Verdict::Decide {
                allow: false,
                remember: false
            }
        );
    }

    #[test]
    fn allow_takes_a_deliberate_move_then_enter() {
        let mut c = Confirm::new(true);
        assert_eq!(c.key(Nav::Right), Verdict::Stay);
        assert_eq!(c.key(Nav::Remember), Verdict::Stay);
        assert_eq!(
            c.key(Nav::Enter),
            Verdict::Decide {
                allow: true,
                remember: true
            }
        );
        c.key(Nav::Next);
        assert_eq!(c.focus, Choice::Deny);
        c.key(Nav::Prev);
        assert_eq!(c.focus, Choice::Allow);
    }

    #[test]
    fn remember_is_ignored_when_the_request_does_not_offer_it() {
        let mut c = Confirm::new(false);
        c.key(Nav::Remember);
        assert!(!c.remember);
        c.set_remember(true);
        assert_eq!(
            c.decide(Choice::Allow),
            Verdict::Decide {
                allow: true,
                remember: false
            }
        );
    }

    #[test]
    fn permission_and_email_replies_map_the_verdict() {
        let req = |allow_remember| PaneRequest::Permission {
            requester: requester(),
            interface: "org.freedesktop.impl.portal.Location".into(),
            title: "Location".into(),
            detail: "d".into(),
            allow_remember,
        };
        let mut p = Pane::open(req(true), ident(), [0; 3]).pane;
        assert_eq!(
            p.key(Nav::Enter),
            Some(PaneReply::Permission {
                allow: false,
                remember: false
            })
        );
        p.key(Nav::Right);
        p.key(Nav::Remember);
        assert_eq!(
            p.key(Nav::Enter),
            Some(PaneReply::Permission {
                allow: true,
                remember: true
            })
        );
        let mut e = Pane::open(
            PaneRequest::Email {
                requester: requester(),
                addresses: vec!["a@b.c".into()],
            },
            ident(),
            [0; 3],
        )
        .pane;
        assert_eq!(e.key(Nav::Enter), Some(PaneReply::Email { send: false }));
        e.key(Nav::Right);
        assert_eq!(e.key(Nav::Enter), Some(PaneReply::Email { send: true }));
        assert_eq!(e.key(Nav::Escape), Some(PaneReply::Cancelled));
    }

    #[test]
    fn screenshot_modes_move_and_reply() {
        let mut s = ShotPicker::new();
        assert_eq!(s.mode, ScreenshotMode::Screen);
        s.key(Nav::Left);
        assert_eq!(s.mode, ScreenshotMode::Screen);
        s.key(Nav::Right);
        s.key(Nav::Right);
        s.key(Nav::Right);
        assert_eq!(s.mode, ScreenshotMode::Area);
        s.key(Nav::Next);
        assert_eq!(s.mode, ScreenshotMode::Screen);
        s.key(Nav::Prev);
        assert_eq!(s.mode, ScreenshotMode::Area);
        assert_eq!(
            s.key(Nav::Enter),
            Some(PaneReply::ScreenshotMode(ScreenshotMode::Area))
        );
    }

    #[test]
    fn hex_parses_with_or_without_hash_and_nothing_else() {
        assert_eq!(parse_hex("#f2c33c"), Some([0xf2, 0xc3, 0x3c]));
        assert_eq!(parse_hex("F2C33C"), Some([0xf2, 0xc3, 0x3c]));
        assert_eq!(parse_hex("#f2c33"), None);
        assert_eq!(parse_hex("#f2c33cff"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex("#é2c33"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn colour_field_and_picker_follow_each_other() {
        let mut c = ColorPick::new([255, 0, 0]);
        assert_eq!(c.draft, "#ff0000");
        c.set_draft("#00ff00".into());
        assert_eq!(c.rgba(), [0, 255, 0, 255]);
        // A half-typed draft keeps the last good colour and will not commit.
        c.set_draft("#00ff".into());
        assert_eq!(c.rgba(), [0, 255, 0, 255]);
        assert!(!c.draft_valid());
        assert_eq!(c.key(Nav::Enter), None);
        c.set_rgba([0, 0, 255, 17]);
        assert_eq!(c.draft, "#0000ff");
        assert_eq!(c.rgba()[3], 255);
        assert_eq!(
            c.key(Nav::Enter),
            Some(PaneReply::Color {
                r: 0.0,
                g: 0.0,
                b: 1.0
            })
        );
    }

    #[test]
    fn account_reply_trims_and_needs_a_name() {
        let mut a = AccountForm::new(
            "  risk ".into(),
            " Chase  ".into(),
            Some("/home/risk/.face".into()),
        );
        assert_eq!(
            a.key(Nav::Enter),
            Some(PaneReply::Account {
                name: "risk".into(),
                real_name: "Chase".into(),
                image: Some("/home/risk/.face".into())
            })
        );
        a.name = "   ".into();
        assert_eq!(a.key(Nav::Enter), None);
    }

    #[test]
    fn passwd_entry_finds_the_uid() {
        let p = "root:x:0:0:root:/root:/bin/bash\nrisk:x:1000:1000:Chase Brown,,,:/home/risk:/bin/zsh\nbare:x:1001:1001::/h:/bin/sh\n";
        assert_eq!(
            passwd_entry(p, 1000),
            Some(("risk".into(), "Chase Brown".into()))
        );
        assert_eq!(passwd_entry(p, 1001), Some(("bare".into(), "bare".into())));
        assert_eq!(passwd_entry(p, 5), None);
        assert_eq!(passwd_entry("garbage", 0), None);
    }

    #[test]
    fn requester_text_cannot_break_the_layout() {
        assert_eq!(clean("a\nb\t c", 20), "a b c");
        assert_eq!(clean("  lead  trail  ", 20), "lead trail");
        assert_eq!(clean("\u{202e}x", 20), "x");
        let long = clean(&"x".repeat(100), 10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
        assert_eq!(clean("", 5), "");
    }

    #[test]
    fn opening_cleans_what_the_requester_wrote() {
        let o = Pane::open(
            PaneRequest::Permission {
                requester: requester(),
                interface: "i".into(),
                title: "T\nitle".into(),
                detail: "Press Enter\nto allow".into(),
                allow_remember: false,
            },
            ident(),
            [0; 3],
        );
        match o.pane {
            Pane::Permission { title, detail, .. } => {
                assert_eq!(title, "T itle");
                assert_eq!(detail, "Press Enter to allow");
            }
            other => panic!("{other:?}"),
        }
    }

    fn requester() -> Requester {
        Requester {
            app_id: "org.example.App".into(),
            display_name: Some("Example".into()),
            icon: None,
            source: IdentitySource::Flatpak,
        }
    }

    fn ident() -> (String, String, Option<String>) {
        ("risk".into(), "Chase".into(), None)
    }
}
