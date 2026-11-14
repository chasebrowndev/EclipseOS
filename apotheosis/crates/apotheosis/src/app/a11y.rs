// SPDX-License-Identifier: AGPL-3.0-only
//! Accessibility (T-20): the AccessKit tree of the window, built only while an assistive
//! technology is present. A timer at `accessibility.update-interval` snapshots the shown
//! panes on a worker (under the taker lock, like render), main builds the model and the
//! builder sends only what changed. Pane text goes to the AT and nowhere else: never logged.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use apoth_a11y::accesskit::Rect as AkRect;
use apoth_a11y::accesskit::{Action as AkAction, ActionRequest};
use apoth_a11y::ids::{self, Target};
use apoth_a11y::{
    Adapter, BarButtonModel, BarModel, Builder, ChipModel, Notify, PaletteModel, PaneModel,
    ProgressKind, ProgressModel, Request, RowText, SearchModel, TabModel, TextModel, TooltipModel,
    WindowModel,
};
use apoth_grid::{SnapRow, WordChars};
use apoth_mux::TabTarget;
use apoth_ui::bar::{BarKind, Key as BarKey};
use apoth_ui::searchbar::{Chip, CountKind};
use apoth_ui::tabs::Badge;
use calloop::RegistrationToken;
use calloop::timer::{TimeoutAction, Timer};

use super::App;
use crate::pane::{MainMsg, PaneKey, PaneShared};

/// What reaches main from the adapter thread and the snapshot worker.
#[derive(Debug)]
pub(crate) enum A11yMsg {
    /// The adapter asked for something.
    Req(Request),
    /// Visible rows of the shown panes.
    Snap(Vec<Snap>),
}

/// One pane's visible text.
#[derive(Debug)]
pub(crate) struct Snap {
    id: PaneKey,
    rows: Vec<RowText>,
    cursor: Option<(usize, usize)>,
}

/// Accessibility state on main.
#[derive(Debug, Default)]
pub(crate) struct A11yState {
    adapter: Option<Adapter>,
    /// An AT is present and the tree is live.
    builder: Option<Builder>,
    /// The next update must be a full tree.
    full: bool,
    timer: Option<RegistrationToken>,
    /// A snapshot job is out.
    inflight: bool,
    announcement: String,
}

/// Forwards adapter requests to main (the sender is behind a mutex: `Notify` is `Sync`).
struct ToMain(Mutex<calloop::channel::Sender<MainMsg>>);

impl Notify for ToMain {
    fn notify(&self, r: Request) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(MainMsg::A11y(A11yMsg::Req(r)));
        }
    }
}

impl App {
    fn a11y_wanted(&self) -> bool {
        // `on` cannot force a tree on: accesskit_unix builds one only once an AT asks.
        self.reloader.config().str("accessibility.enabled") != Some("off")
    }

    /// Register with the accessibility bus unless `accessibility.enabled` is `off`.
    pub(super) fn start_a11y(&mut self) {
        if self.a11y.adapter.is_some() || !self.a11y_wanted() {
            return;
        }
        let notify = ToMain(Mutex::new(self.to_main.clone()));
        let mut adapter = Adapter::new(notify);
        adapter.window_focus(self.win_focused);
        self.a11y.adapter = Some(adapter);
    }

    /// A reload may have turned the tree on or off.
    pub(super) fn a11y_reconfigure(&mut self) {
        if self.a11y_wanted() {
            self.start_a11y();
            if let Some(b) = self.a11y.builder.as_mut() {
                b.configure(
                    TextModel::Runs,
                    WordChars::new(&self.settings.input.word_extra),
                );
                self.a11y.full = true;
            }
        } else {
            self.a11y_stop();
            self.a11y.adapter = None;
        }
    }

    fn a11y_stop(&mut self) {
        self.a11y.builder = None;
        self.a11y.inflight = false;
        if let Some(t) = self.a11y.timer.take() {
            self.lh.remove(t);
        }
    }

    pub(super) fn on_a11y(&mut self, m: A11yMsg) {
        match m {
            A11yMsg::Req(Request::Activate) => {
                if self.a11y.adapter.is_none() {
                    return;
                }
                self.a11y.builder = Some(Builder::new(
                    TextModel::Runs,
                    WordChars::new(&self.settings.input.word_extra),
                ));
                self.a11y.full = true;
                self.a11y_start_timer();
                self.a11y_snapshot();
            }
            A11yMsg::Req(Request::Deactivate) => self.a11y_stop(),
            A11yMsg::Req(Request::Action(r)) => self.a11y_action(&r),
            A11yMsg::Snap(snaps) => {
                self.a11y.inflight = false;
                self.a11y_publish(snaps);
            }
        }
    }

    fn a11y_start_timer(&mut self) {
        if self.a11y.timer.is_some() {
            return;
        }
        let ms = self
            .reloader
            .config()
            .int("accessibility.update-interval")
            .clamp(10, 5000) as u64;
        let period = Duration::from_millis(ms);
        let tok =
            self.lh
                .insert_source(Timer::from_duration(period), move |_, (), app: &mut App| {
                    if app.a11y.builder.is_none() {
                        app.a11y.timer = None;
                        return TimeoutAction::Drop;
                    }
                    app.a11y_snapshot();
                    TimeoutAction::ToDuration(period)
                });
        match tok {
            Ok(t) => self.a11y.timer = Some(t),
            Err(e) => tracing::warn!(error = %e.error, "cannot start the accessibility timer"),
        }
    }

    /// Snapshot the shown panes on a worker.
    pub(super) fn a11y_snapshot(&mut self) {
        if self.a11y.builder.is_none() || self.a11y.inflight {
            return;
        }
        let shared: Vec<(PaneKey, Arc<PaneShared>)> = self
            .placed
            .iter()
            .filter_map(|p| self.panes.get(&p.id).map(|x| (p.id, Arc::clone(&x.shared))))
            .collect();
        let tx = self.to_main.clone();
        let job: crate::workers::Job = Box::new(move || {
            let mut row = SnapRow::default();
            let snaps = shared
                .iter()
                .map(|(id, s)| {
                    let t = s.lock();
                    let g = t.grid();
                    let rows = (0..g.rows())
                        .map(|y| {
                            g.copy_row(y, &mut row);
                            RowText::from_snap(&row)
                        })
                        .collect();
                    let c = t.cursor();
                    Snap {
                        id: *id,
                        rows,
                        cursor: c.visible.then_some((c.y, c.x)),
                    }
                })
                .collect();
            let _ = tx.send(MainMsg::A11y(A11yMsg::Snap(snaps)));
        });
        if self.workers.submit(job).is_ok() {
            self.a11y.inflight = true;
        }
    }

    fn a11y_publish(&mut self, snaps: Vec<Snap>) {
        let Some(m) = self.a11y_model(snaps) else {
            return;
        };
        let Some(b) = self.a11y.builder.as_mut() else {
            return;
        };
        let u = if std::mem::take(&mut self.a11y.full) {
            Some(b.full(&m))
        } else {
            b.update(&m)
        };
        if let (Some(u), Some(a)) = (u, self.a11y.adapter.as_mut()) {
            a.update(u);
        }
    }

    fn a11y_model(&self, snaps: Vec<Snap>) -> Option<WindowModel> {
        let w = self.session.window(self.mux_win)?;
        let geo = self.geo;
        // Interface units to the logical window units the panes use.
        let k = geo.map_or(1.0, |g| f64::from(g.ui) / self.scale().max(0.1));
        let rect = |r: apoth_ui::Rect| AkRect {
            x0: f64::from(r.x) * k,
            y0: f64::from(r.y) * k,
            x1: f64::from(r.right()) * k,
            y1: f64::from(r.bottom()) * k,
        };
        let specs = self.tab_specs();
        let strip = geo.and_then(|g| g.strip);
        let strip_l = strip.map(|r| apoth_ui::tabs::layout(r, crate::ui::STRIP_ID, &specs));
        let tabs = w
            .tabs
            .iter()
            .enumerate()
            .map(|(i, &tid)| {
                let cell = strip_l
                    .as_ref()
                    .and_then(|l| l.cells.iter().find(|c| c.index == i));
                let badges = self.tab_badges(tid);
                let words = badge_words(&badges);
                let pane = self.session.tab(tid).and_then(|t| t.focused());
                TabModel {
                    id: tid,
                    label: self.tab_title(tid),
                    selected: Some(tid) == w.active,
                    description: (!words.is_empty()).then(|| words.join(", ")),
                    bounds: cell.map(|c| rect(c.rect)),
                    close: cell.map(|c| rect(c.close)),
                    progress: badges.iter().find_map(|b| badge_progress(*b)),
                    profile: pane.and_then(|p| self.pane_profile(p)).map(str::to_owned),
                }
            })
            .collect();
        let focus = self.focused();
        let scale = self.scale().max(0.1);
        let cell = (
            f64::from(self.cell.0) / scale,
            f64::from(self.cell.1) / scale,
        );
        let panes = snaps
            .into_iter()
            .filter_map(|s| {
                let p = self.placed.iter().find(|p| p.id == s.id)?;
                Some(PaneModel {
                    id: s.id,
                    label: self
                        .panes
                        .get(&s.id)
                        .map(|x| x.title.clone())
                        .unwrap_or_default(),
                    focused: Some(s.id) == focus,
                    origin: (f64::from(p.origin.0) / scale, f64::from(p.origin.1) / scale),
                    cell,
                    cols: u16::try_from(p.dims.1).unwrap_or(u16::MAX),
                    rows: s.rows,
                    cursor: s.cursor,
                    // No credential signal reaches the terminal yet (reported gap).
                    credential: false,
                })
            })
            .collect();
        let palette = self.palette.as_ref().map(|p| {
            let (rows, selected) = p.window(self.palette_rows());
            PaletteModel {
                query: p.query.clone(),
                placeholder: p.mode.placeholder().to_owned(),
                rows: rows.iter().map(|r| r.title.clone()).collect(),
                selected,
            }
        });
        let bar = self.bar.as_ref().map(|o| {
            let l =
                geo.map(|g| apoth_ui::bar::layout(g.area, &o.spec, &apoth_ui::text::geometry()));
            let guard = o.shown_at.elapsed().as_millis()
                < u128::from(apoth_ui::tokens::motion::CONSENT_GUARD_MS);
            BarModel {
                headline: o.spec.headline.text.clone(),
                detail: o.spec.detail.as_ref().map(|d| d.text.clone()),
                body: o.spec.body.as_ref().map(|b| b.text.clone()),
                alert: o.spec.kind == BarKind::Error,
                buttons: o
                    .spec
                    .buttons
                    .iter()
                    .enumerate()
                    .map(|(i, b)| BarButtonModel {
                        label: b.label.clone(),
                        key: match b.key {
                            BarKey::Char(c) => c.to_ascii_uppercase().to_string(),
                            BarKey::Escape => "Escape".to_owned(),
                        },
                        disabled: guard,
                        bounds: l
                            .as_ref()
                            .and_then(|l| l.buttons.get(i))
                            .map(|c| rect(c.rect)),
                    })
                    .collect(),
                bounds: l.as_ref().map(|l| rect(l.bar)),
                body_bounds: l.as_ref().and_then(|l| l.body).map(rect),
            }
        });
        let search = self.search_spec().map(|sp| {
            let l = self.search_layout();
            SearchModel {
                query: sp.query.clone(),
                placeholder: "Find".to_owned(),
                count: sp.count.clone(),
                error: sp.count_kind == CountKind::Error,
                focused: sp.focused,
                can_step: sp.can_step,
                chips: sp
                    .chips
                    .iter()
                    .map(|c| {
                        let (name, description) = chip_words(c.chip, &c.label, c.on);
                        ChipModel {
                            name,
                            description,
                            on: c.on,
                            bounds: l.as_ref().and_then(|l| {
                                l.chips.iter().find(|x| x.0 == c.chip).map(|x| rect(x.2))
                            }),
                        }
                    })
                    .collect(),
                bounds: l.as_ref().map(|l| rect(l.sheet)),
                field: l.as_ref().map(|l| rect(l.field)),
                prev: l.as_ref().map(|l| rect(l.prev)),
                next: l.as_ref().map(|l| rect(l.next)),
                close: l.as_ref().map(|l| rect(l.close)),
            }
        });
        let tooltip = self.tooltip_spec().map(|t| TooltipModel {
            bounds: geo
                .map(|g| apoth_ui::tooltip::layout(g.area, &t, &apoth_ui::text::geometry()))
                .map(rect),
            text: t.text.text,
        });
        Some(WindowModel {
            title: self.window_title_now(),
            tabs,
            panes,
            palette,
            announcement: self.a11y.announcement.clone(),
            strip: strip.map(rect),
            new_tab: strip_l.as_ref().map(|l| rect(l.new_tab)),
            bar,
            search,
            tooltip,
        })
    }

    /// An AT asked for an action. Every action runs the handler the mouse click runs; a
    /// modal sheet owns the interface, as it owns the pointer.
    fn a11y_action(&mut self, r: &ActionRequest) {
        if self.a11y.builder.is_none() || !matches!(r.action, AkAction::Click | AkAction::Focus) {
            return;
        }
        let target = ids::target(r.target_node);
        let modal = self.bar.as_ref().is_some_and(|o| !o.takes_room());
        if modal && !matches!(target, Target::BarButton(_)) {
            return;
        }
        match target {
            Target::Tab(tid) => {
                let i = self
                    .session
                    .window(self.mux_win)
                    .and_then(|w| w.tabs.iter().position(|t| *t == tid));
                if let Some(i) = i {
                    self.focus_tab(TabTarget::Index(i));
                }
            }
            Target::TabClose(tid) if r.action == AkAction::Click => {
                if self
                    .session
                    .window(self.mux_win)
                    .is_some_and(|w| w.tabs.contains(&tid))
                {
                    self.close_tab(tid);
                }
            }
            Target::NewTab if r.action == AkAction::Click => self.new_tab(None, None),
            Target::BarButton(i) if r.action == AkAction::Click => {
                // The 500 ms input guard applies to an accessible click exactly as to a press.
                let ready = self.bar.as_ref().is_some_and(|o| {
                    i < o.spec.buttons.len()
                        && o.shown_at.elapsed().as_millis()
                            >= u128::from(apoth_ui::tokens::motion::CONSENT_GUARD_MS)
                });
                if ready {
                    self.bar_click_answer(i);
                }
            }
            Target::SearchField => {
                self.search_focus();
                self.send_layout();
            }
            Target::SearchChip(i) if r.action == AkAction::Click => {
                let chip = self
                    .search_spec()
                    .and_then(|s| s.chips.get(i).map(|c| c.chip));
                if let Some(c) = chip {
                    self.search_chip(c);
                }
            }
            Target::SearchPrev | Target::SearchNext if r.action == AkAction::Click => {
                if self.nav.search.is_some() {
                    self.search_step(target == Target::SearchNext);
                    self.send_layout();
                }
            }
            Target::SearchClose if r.action == AkAction::Click => self.close_search(),
            Target::Pane(id) => {
                // Run and line ids carry the low 32 bits of the pane id.
                let key = self
                    .panes
                    .keys()
                    .copied()
                    .find(|k| *k == id || (*k & 0xffff_ffff) == id);
                if let Some(k) = key {
                    self.reveal(k);
                }
            }
            Target::PaletteRow(i) => {
                let window = self.palette_rows();
                let Some(p) = self.palette.as_mut() else {
                    return;
                };
                let abs = p.scroll + i;
                p.select(abs, window);
                if r.action == AkAction::Click {
                    self.palette_pick();
                } else {
                    self.send_layout();
                }
            }
            _ => {}
        }
        self.a11y_snapshot();
    }

    /// Speak a short interface message (bell, events) through the live region.
    pub(super) fn a11y_announce(&mut self, text: &str) {
        if self.a11y.builder.is_none() || !self.reloader.config().bool("accessibility.announce") {
            return;
        }
        // The same text twice in a row would not change the node; alternate a trailing space.
        self.a11y.announcement = if self.a11y.announcement.trim_end() == text
            && !self.a11y.announcement.ends_with(' ')
        {
            format!("{text} ")
        } else {
            text.to_owned()
        };
        self.a11y_snapshot();
    }

    /// The window gained or lost keyboard focus.
    pub(super) fn a11y_window_focus(&mut self, focused: bool) {
        if let Some(a) = self.a11y.adapter.as_mut() {
            a.window_focus(focused);
        }
    }
}

/// The badges as words for the tab's description (progress has its own node).
fn badge_words(badges: &[Badge]) -> Vec<String> {
    badges
        .iter()
        .filter_map(|b| match b {
            Badge::Unseen => Some("new output".to_owned()),
            Badge::Bell => Some("bell".to_owned()),
            Badge::Running => Some("running".to_owned()),
            Badge::Exit(0) => Some("last command succeeded".to_owned()),
            Badge::Exit(n) => Some(format!("last command exited {n}")),
            _ => None,
        })
        .collect()
}

fn badge_progress(b: Badge) -> Option<ProgressModel> {
    let (kind, value) = match b {
        Badge::Progress(v) => (ProgressKind::Normal, v),
        Badge::ProgressError(v) => (ProgressKind::Error, v),
        Badge::ProgressPaused(v) => (ProgressKind::Paused, v),
        Badge::ProgressBusy => (ProgressKind::Busy, 0),
        _ => return None,
    };
    Some(ProgressModel { kind, value })
}

/// The spoken name of a search chip, and its state in words when on/off is not enough.
fn chip_words(chip: Chip, label: &str, on: bool) -> (String, Option<String>) {
    match chip {
        Chip::Case => (
            "Match case".to_owned(),
            Some(
                match (on, label) {
                    (false, _) => "smart case",
                    (true, "aa") => "case insensitive",
                    (true, _) => "case sensitive",
                }
                .to_owned(),
            ),
        ),
        Chip::Regex => ("Regular expression".to_owned(), None),
        Chip::Word => ("Whole word".to_owned(), None),
        Chip::Scope => ("Screen only".to_owned(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_have_spoken_names() {
        assert_eq!(chip_words(Chip::Regex, ".*", true).0, "Regular expression");
        assert_eq!(chip_words(Chip::Word, "\\b", false).0, "Whole word");
        assert_eq!(chip_words(Chip::Scope, "Screen", true).0, "Screen only");
        let d = |l, on| chip_words(Chip::Case, l, on).1;
        assert_eq!(d("Aa", false).as_deref(), Some("smart case"));
        assert_eq!(d("Aa", true).as_deref(), Some("case sensitive"));
        assert_eq!(d("aa", true).as_deref(), Some("case insensitive"));
    }

    #[test]
    fn badges_become_words_and_progress() {
        let b = [
            Badge::Bell,
            Badge::ProgressError(40),
            Badge::Exit(2),
            Badge::Exit(0),
        ];
        assert_eq!(
            badge_words(&b),
            ["bell", "last command exited 2", "last command succeeded"]
        );
        assert_eq!(
            b.iter().find_map(|x| badge_progress(*x)),
            Some(ProgressModel {
                kind: ProgressKind::Error,
                value: 40
            })
        );
        assert_eq!(badge_progress(Badge::Bell), None);
        assert_eq!(
            badge_progress(Badge::ProgressBusy).map(|p| p.kind),
            Some(ProgressKind::Busy)
        );
    }
}
