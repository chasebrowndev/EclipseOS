// SPDX-License-Identifier: AGPL-3.0-only
//! Power: the session's own actions and the Eclipse update that ends with a
//! restart (ADR 0065 amendment, ADR 0085). The cell is one understated glyph;
//! clicking it opens the power drawer.
//!
//! The drawer lists Lock, Log out, Suspend, Reboot and Power off, each greyed
//! by logind's answer (`Availability`). Log out, Reboot and Power off ask for a
//! second click, the row turning into "Confirm …" with a Cancel beside it.
//! Below a hairline, and only while `ec_services::updates` reports a pending
//! Eclipse package, sit "Update & reboot" and "Update & power off"; choosing
//! one opens a scope prompt, and Confirm hands the scope and the power action
//! to the backend. The backend is the only thing here that runs pacman, and it
//! powers the machine off only on success. Nothing in this module calls a power
//! action directly except a machine row the human confirmed.
//!
//! A status widget: the cell reads no service. The state lives in [`Menu`] on
//! the app, fed by the services' drain, and the sheet is a pure function of it
//! and the progress spinner's phase.
//!
//! Ledger: none. Availability is shown by grey, the pending update by plain
//! ink, and the update in flight by a spinner that points at the progress cell.
//! The strip already carries the one live yellow.

use iced::{Color, Element, Theme};

use ec_services::session::{Action, Availability};
use ec_services::updates::{After, Event, Scope, Update};
use ec_ui::tokens::{bar, color, drawer};
use ec_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{elide, Sheet, DRAWER_ERR_CHARS};

/// The cell's glyph: the freedesktop system-shutdown mark, the usual power symbol.
const CELL_ICON: &str = "system-shutdown";

/// The machine's own actions, in the order the drawer lists them.
const MACHINE: [Action; 5] = [
    Action::Lock,
    Action::LogOut,
    Action::Suspend,
    Action::Reboot,
    Action::PowerOff,
];

/// The pending Eclipse update, as the drawer shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    /// The packages the Eclipse-only scope would upgrade (the closure).
    pub packages: usize,
    /// Every pending package, for the `All` scope.
    pub total: usize,
}

/// The backend's apply, as far as the drawer has been told.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Apply {
    #[default]
    Idle,
    /// The upgrade is running; the power action follows it.
    Running(After),
    /// The upgrade succeeded and the power action was requested.
    Installed(After),
    /// The last attempt ended here. The reason is the backend's own words.
    Failed(String),
}

/// What the power drawer knows, and what the human has started and not yet
/// finished. Lives on the app; reset whenever the drawer closes.
#[derive(Debug)]
pub struct Menu {
    /// logind's answer per action, from the last probe. Empty until it answers,
    /// and then every action is greyed as "checking".
    avail: Vec<(Action, Availability)>,
    /// The pending update's counts. `None` means the update rows are not
    /// offered at all.
    pub update: Option<Pending>,
    /// The destructive row awaiting its second click.
    pub confirm: Option<Action>,
    /// The update whose scope prompt is open.
    pub scope_for: Option<After>,
    /// The scope the prompt has picked.
    pub scope: Scope,
    pub apply: Apply,
    /// The reason logind (or polkit) refused the last power action, until the
    /// human asks for another.
    pub refused: Option<String>,
}

impl Default for Menu {
    fn default() -> Self {
        Menu {
            avail: Vec::new(),
            update: None,
            confirm: None,
            scope_for: None,
            scope: Scope::EclipseAndDeps,
            apply: Apply::Idle,
            refused: None,
        }
    }
}

impl Menu {
    /// logind's answer for `action`, `None` until the probe has answered.
    pub fn answer(&self, action: Action) -> Option<Availability> {
        self.avail
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, availability)| *availability)
    }

    /// Replace the probe's answers.
    pub fn set_answers(&mut self, answers: Vec<(Action, Availability)>) {
        self.avail = answers;
    }

    /// Whether logind offers `action` at all (Yes or Challenge).
    fn offered(&self, action: Action) -> bool {
        self.answer(action).is_some_and(Availability::offered)
    }

    /// An apply is in flight, or has installed and is restarting: every power
    /// row waits for it.
    pub fn busy(&self) -> bool {
        matches!(self.apply, Apply::Running(_) | Apply::Installed(_))
    }

    /// A machine row the human may press now.
    pub fn enabled(&self, action: Action) -> bool {
        !self.busy() && self.offered(action)
    }

    /// An update row the human may press now: the update is pending, nothing
    /// is in flight, and its power action is one logind offers.
    pub fn can_update(&self, then: After) -> bool {
        self.update.is_some() && !self.busy() && self.offered(then_action(then))
    }

    /// Forget a confirmation and an open scope prompt. Called when the drawer
    /// closes, so a reopened drawer starts from its rows. The refusal stays
    /// until the human asks for another action.
    pub fn reset(&mut self) {
        self.confirm = None;
        self.scope_for = None;
    }

    /// The human pressed a machine row. Returns `true` when the action should
    /// run now. A destructive action asks first: its first press makes it the
    /// pending confirmation and returns `false`. Nothing runs that the menu did
    /// not offer.
    pub fn press(&mut self, action: Action) -> bool {
        if !self.enabled(action) {
            return false;
        }
        if is_destructive(action) && self.confirm != Some(action) {
            self.confirm = Some(action);
            return false;
        }
        self.confirm = None;
        self.refused = None;
        true
    }

    /// Fold one update from the service: the pending counts, or an event of the
    /// apply it was asked for.
    pub fn fold_update(&mut self, update: Update) {
        match update {
            Update::Pending {
                total,
                eclipse,
                eclipse_with_deps,
            } => {
                self.update = (!eclipse.is_empty()).then_some(Pending {
                    packages: eclipse_with_deps.len(),
                    total,
                });
            }
            Update::Unavailable => self.update = None,
            Update::Event(event) => match event {
                Event::Started { then, .. } => self.apply = Apply::Running(then),
                Event::Refused { reason } | Event::Failed { reason } => self.apply = Apply::Failed(reason),
                Event::Installed { then } => self.apply = Apply::Installed(then),
            },
        }
    }

    /// Start an apply the scope prompt confirmed. The backend's own Started
    /// event follows; this only makes the drawer say so at once.
    pub fn begin_apply(&mut self, then: After) {
        self.scope_for = None;
        self.refused = None;
        self.apply = Apply::Running(then);
    }
}

/// Whether an action ends the session or the machine, and so needs its second
/// click. Lock and Suspend are one press: they do not lose the human's work.
fn is_destructive(action: Action) -> bool {
    matches!(action, Action::LogOut | Action::Reboot | Action::PowerOff)
}

/// The logind action an update finishes with.
fn then_action(then: After) -> Action {
    match then {
        After::Reboot => Action::Reboot,
        After::PowerOff => Action::PowerOff,
    }
}

fn label(action: Action) -> &'static str {
    match action {
        Action::Lock => "Lock",
        Action::LogOut => "Log out",
        Action::Suspend => "Suspend",
        Action::Hibernate => "Hibernate",
        Action::Reboot => "Reboot",
        Action::PowerOff => "Power off",
    }
}

fn confirm_label(action: Action) -> String {
    format!("Confirm {}", label(action).to_lowercase())
}

fn icon(action: Action) -> &'static str {
    match action {
        Action::Lock => "system-lock-screen",
        Action::LogOut => "system-log-out",
        Action::Suspend | Action::Hibernate => "weather-clear-night",
        Action::Reboot => "system-reboot",
        Action::PowerOff => "system-shutdown",
    }
}

fn update_label(then: After) -> &'static str {
    match then {
        After::Reboot => "Update & reboot",
        After::PowerOff => "Update & power off",
    }
}

/// The verb the scope prompt's Confirm names: the whole action, in full.
fn update_verb(then: After) -> &'static str {
    match then {
        After::Reboot => "update & reboot",
        After::PowerOff => "update & power off",
    }
}

/// What the drawer says a row cannot do, as its mono note. `None` for a row
/// logind offers as it is.
fn availability_note(answer: Option<Availability>) -> Option<&'static str> {
    match answer {
        None => Some("checking"),
        Some(Availability::Yes) => None,
        Some(Availability::Challenge) => Some("asks first"),
        Some(Availability::No) => Some("not permitted"),
    }
}

fn packages(n: usize) -> String {
    if n == 1 {
        "1 package".to_owned()
    } else {
        format!("{n} packages")
    }
}

/// The cell takes the glyph's own width and is always present: the drawer is
/// where a machine that refuses an action says so.
pub fn spans(_app: &App) -> Spans {
    Spans {
        core: bar::MARK,
        revealed: 0.0,
        lead: 0.0,
        present: true,
    }
}

pub fn view(_app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let mark = parts::mark(
        crate::icons::symbolic(CELL_ICON),
        bar::MARK,
        color::text_secondary().scale_alpha(ink),
    );
    Parts {
        core: super::press(mark, bar::MARK, Message::Open(Drawer::Power)),
        revealed: None,
    }
}

/// One machine row: its label and mark, its note, and a press when the menu
/// offers it. A destructive row that is awaiting its second click is the
/// confirmation instead.
fn machine_row(app: &App, action: Action) -> Element<'static, Message, Theme> {
    let menu = &app.power_menu;
    if menu.confirm == Some(action) {
        return parts::drawer_confirm(
            &confirm_label(action),
            true,
            Message::PowerRow(action),
            Message::PowerCancel,
        );
    }
    let enabled = menu.enabled(action);
    let note = if menu.busy() {
        None
    } else {
        availability_note(menu.answer(action))
    };
    let press = enabled.then(|| Message::PowerRow(action));
    let mark = parts::mark(crate::icons::symbolic(icon(action)), drawer::MARK, ink(enabled));
    parts::drawer_action(mark, label(action), note, press)
}

/// A row's mark ink: the enabled ink for a row that answers, the tertiary
/// ink for one greyed out.
fn ink(enabled: bool) -> Color {
    if enabled {
        color::text_secondary()
    } else {
        color::text_tertiary()
    }
}

/// One update row. While its apply runs it is the spinner that points at the
/// progress cell; the other one waits.
fn update_row(app: &App, then: After) -> Element<'static, Message, Theme> {
    let menu = &app.power_menu;
    let mark_icon = crate::icons::symbolic("software-update-available");
    match &menu.apply {
        Apply::Running(t) if *t == then => parts::drawer_pending(
            parts::mark(mark_icon, drawer::MARK, color::text()),
            update_label(then),
            "see progress",
            app.progress_phase(),
        ),
        Apply::Installed(t) if *t == then => parts::drawer_pending(
            parts::mark(mark_icon, drawer::MARK, color::text()),
            match then {
                After::Reboot => "Restarting",
                After::PowerOff => "Powering off",
            },
            "installed",
            app.progress_phase(),
        ),
        _ => {
            let enabled = menu.can_update(then);
            let note = if menu.busy() {
                None
            } else {
                availability_note(menu.answer(then_action(then)))
            };
            let press = enabled.then_some(Message::PowerUpdate(then));
            parts::drawer_action(
                parts::mark(mark_icon, drawer::MARK, ink(enabled)),
                update_label(then),
                note,
                press,
            )
        }
    }
}

/// The scope prompt: two radio options, a note that names the partial
/// upgrade, and the Confirm that starts it.
fn scope_prompt(s: &mut Sheet, menu: &Menu, then: After) {
    let (packages_n, total) = menu.update.map_or((0, 0), |p| (p.packages, p.total));
    s.section("update scope");
    s.block(
        parts::drawer_option(
            menu.scope == Scope::EclipseAndDeps,
            &format!("Eclipse and dependencies only ({})", packages(packages_n)),
            Message::PowerScope(Scope::EclipseAndDeps),
        ),
        drawer::OPTION_H,
    );
    s.block(
        parts::drawer_option(
            menu.scope == Scope::All,
            &format!("All packages ({total})"),
            Message::PowerScope(Scope::All),
        ),
        drawer::OPTION_H,
    );
    s.row(parts::drawer_note("the first is a partial upgrade"));
    s.row(parts::drawer_confirm(
        &format!("Confirm {}", update_verb(then)),
        true,
        Message::PowerApply,
        Message::PowerScopeCancel,
    ));
}

/// The power drawer. Anatomy: the head; the five machine rows; below a
/// hairline, the update rows, or the scope prompt that replaces them; the
/// backend's failure and any logind refusal, in danger ink, at the foot.
///
/// Ledger: none. Nothing here is a live magnitude; the update is a state, and
/// the spinner is neutral ink.
pub(crate) fn sheet(app: &App) -> Sheet {
    let menu = &app.power_menu;
    let mut s = Sheet::new(parts::drawer_sheet_head("power"), drawer::HEAD_H);
    for action in MACHINE {
        s.row(machine_row(app, action));
    }
    if menu.update.is_some() {
        s.rule();
        match menu.scope_for {
            Some(then) => scope_prompt(&mut s, menu, then),
            None => {
                s.row(update_row(app, After::Reboot));
                s.row(update_row(app, After::PowerOff));
            }
        }
    }
    if let Apply::Failed(reason) = &menu.apply {
        let line = format!("couldn't update: {reason}");
        s.row(parts::drawer_error(&elide(&line, DRAWER_ERR_CHARS)));
    }
    if let Some(reason) = &menu.refused {
        let line = format!("couldn't: {reason}");
        s.row(parts::drawer_error(&elide(&line, DRAWER_ERR_CHARS)));
    }
    s
}

/// The fixture for `HYPERION_PREVIEW=power`: which state the drawer shows,
/// from `HYPERION_PREVIEW_POWER`, and which logind answers, from
/// `HYPERION_PREVIEW_SESSION=refuse`. Debug builds only; a fixture never
/// reaches logind or pkexec, because its messages are dropped before any
/// service call.
///
/// `none` (the default) has no update; `updates` a pending one; `scope` the
/// scope prompt; `confirm` a Reboot awaiting its second click; `running` an
/// apply in flight; `failed` an apply that ended.
#[cfg(debug_assertions)]
pub fn preview(menu: &mut Menu) {
    let refuse = std::env::var("HYPERION_PREVIEW_SESSION").as_deref() == Ok("refuse");
    let answer = |suspend: Availability, reboot: Availability, power_off: Availability| {
        vec![
            (Action::Lock, Availability::Yes),
            (Action::LogOut, Availability::Yes),
            (Action::Suspend, suspend),
            (Action::Reboot, reboot),
            (Action::PowerOff, power_off),
        ]
    };
    menu.set_answers(if refuse {
        answer(Availability::No, Availability::No, Availability::No)
    } else {
        answer(Availability::Challenge, Availability::Yes, Availability::Yes)
    });
    let pending = Pending {
        packages: 4,
        total: 37,
    };
    match std::env::var("HYPERION_PREVIEW_POWER").as_deref() {
        Ok("updates") => menu.update = Some(pending),
        Ok("scope") => {
            menu.update = Some(pending);
            menu.scope_for = Some(After::Reboot);
        }
        Ok("confirm") => menu.confirm = Some(Action::Reboot),
        Ok("running") => {
            menu.update = Some(pending);
            menu.apply = Apply::Running(After::Reboot);
        }
        Ok("failed") => {
            menu.update = Some(pending);
            menu.apply = Apply::Failed("the upgrade exited with status 1".to_owned());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::updates::PendingPkg;

    fn all_answer(answer: Availability) -> Vec<(Action, Availability)> {
        MACHINE.iter().map(|a| (*a, answer)).collect()
    }

    fn menu_with(answer: Availability) -> Menu {
        let mut menu = Menu::default();
        menu.set_answers(all_answer(answer));
        menu
    }

    fn pkg(name: &str) -> PendingPkg {
        PendingPkg {
            name: name.to_owned(),
            old: "1".to_owned(),
            new: "2".to_owned(),
        }
    }

    fn pending(eclipse: usize, closure: usize, total: usize) -> Update {
        Update::Pending {
            total,
            eclipse: (0..eclipse).map(|i| pkg(&format!("eclipseos-{i}"))).collect(),
            eclipse_with_deps: (0..closure).map(|i| pkg(&format!("dep-{i}"))).collect(),
        }
    }

    #[test]
    fn a_destructive_row_asks_before_it_acts() {
        let mut menu = menu_with(Availability::Yes);
        assert!(!menu.press(Action::Reboot), "the first press only asks");
        assert_eq!(menu.confirm, Some(Action::Reboot));
        assert!(menu.press(Action::Reboot), "the second press runs it");
        assert_eq!(menu.confirm, None);
    }

    #[test]
    fn a_second_destructive_row_moves_the_question_and_does_not_run() {
        let mut menu = menu_with(Availability::Yes);
        assert!(!menu.press(Action::Reboot));
        assert!(
            !menu.press(Action::PowerOff),
            "pressing another row asks about it"
        );
        assert_eq!(menu.confirm, Some(Action::PowerOff));
    }

    #[test]
    fn lock_and_suspend_run_on_one_press() {
        let mut menu = menu_with(Availability::Yes);
        assert!(menu.press(Action::Lock));
        assert!(menu.press(Action::Suspend));
    }

    #[test]
    fn nothing_runs_that_logind_did_not_offer() {
        let mut menu = Menu::default();
        assert!(!menu.press(Action::Lock), "no answer yet: checking, not offered");
        menu.set_answers(all_answer(Availability::No));
        assert!(!menu.press(Action::Suspend));
        assert!(
            !menu.press(Action::Reboot),
            "refused, so not even the first press asks"
        );
        assert_eq!(menu.confirm, None);
    }

    #[test]
    fn a_challenge_is_offered_and_the_human_is_asked_by_polkit() {
        let mut menu = Menu::default();
        menu.set_answers(all_answer(Availability::Challenge));
        assert!(menu.press(Action::Suspend));
        assert_eq!(
            availability_note(Some(Availability::Challenge)),
            Some("asks first")
        );
    }

    #[test]
    fn nothing_runs_while_an_apply_is_in_flight() {
        let mut menu = menu_with(Availability::Yes);
        menu.begin_apply(After::Reboot);
        assert!(menu.busy());
        assert!(!menu.press(Action::Lock));
        assert!(!menu.press(Action::Reboot));
        assert!(!menu.can_update(After::PowerOff));
    }

    #[test]
    fn the_update_is_offered_only_while_eclipse_is_pending() {
        let mut menu = menu_with(Availability::Yes);
        menu.fold_update(pending(0, 0, 40));
        assert_eq!(menu.update, None, "no Eclipse package, no update row");
        menu.fold_update(pending(2, 5, 40));
        assert_eq!(
            menu.update,
            Some(Pending {
                packages: 5,
                total: 40
            })
        );
        menu.fold_update(Update::Unavailable);
        assert_eq!(menu.update, None, "checkupdates missing: never offered");
    }

    #[test]
    fn an_update_row_needs_its_power_action_offered() {
        let mut menu = menu_with(Availability::Yes);
        menu.fold_update(pending(1, 1, 3));
        assert!(menu.can_update(After::PowerOff));
        menu.set_answers(vec![(Action::PowerOff, Availability::No)]);
        assert!(
            !menu.can_update(After::PowerOff),
            "a refused power off is not offered"
        );
    }

    #[test]
    fn the_events_drive_the_apply_state() {
        let mut menu = menu_with(Availability::Yes);
        menu.fold_update(Update::Event(Event::Started {
            scope: Scope::All,
            then: After::Reboot,
        }));
        assert_eq!(menu.apply, Apply::Running(After::Reboot));
        menu.fold_update(Update::Event(Event::Failed {
            reason: "the upgrade exited with status 1".into(),
        }));
        assert_eq!(
            menu.apply,
            Apply::Failed("the upgrade exited with status 1".into())
        );
        assert!(!menu.busy(), "a failure re-enables the rows");
        menu.fold_update(Update::Event(Event::Refused {
            reason: "already running".into(),
        }));
        assert_eq!(menu.apply, Apply::Failed("already running".into()));
        menu.fold_update(Update::Event(Event::Installed {
            then: After::PowerOff,
        }));
        assert_eq!(menu.apply, Apply::Installed(After::PowerOff));
        assert!(menu.busy(), "a restart in progress holds the rows");
    }

    #[test]
    fn closing_the_drawer_forgets_a_confirmation_and_an_open_prompt() {
        let mut menu = menu_with(Availability::Yes);
        menu.fold_update(pending(1, 1, 3));
        menu.press(Action::Reboot);
        menu.scope_for = Some(After::Reboot);
        menu.reset();
        assert_eq!(menu.confirm, None);
        assert_eq!(menu.scope_for, None);
    }

    #[test]
    fn the_scope_counts_read_as_packages() {
        assert_eq!(packages(1), "1 package");
        assert_eq!(packages(3), "3 packages");
    }
}
