// SPDX-License-Identifier: AGPL-3.0-only
//! The window menu under `mode "de"` (COMP-05 §7.1, C-24, ADR 0090).
//!
//! A compositor-drawn menu of window operations: Minimize, Maximize/Restore,
//! Shade, Keep above, Keep below, On all desktops, Move to desktop, Close.
//! It is opened from a right press on a titlebar, from `xdg_toplevel.
//! show_window_menu`, and by [`open_window_menu`], which M6 wires to a socket
//! method. smithay 0.7's `XwmHandler` has no window-menu request, so an X11
//! window reaches it through its titlebar only.
//!
//! **This is not trusted UI.** It carries no client text (its rows are fixed
//! words and workspace numbers), draws in the overlay pass after the windows
//! and before trusted UI, and is never in a capture pass list: it is spliced in
//! by the backends beside the Radiant drop guides and `render/capture.rs` does
//! not name it (`render/scan_tests.rs` enforces both). Being compositor-drawn
//! confers no trust.
//!
//! **Its own grab, no client grab.** While open the menu owns the human's
//! pointer and keyboard: motion, buttons, scroll and keys go to it and to no
//! client (the input paths consult [`WinMenu::is_open`] behind every gate that
//! already holds the seat). It never calls `set_grab` on the keyboard, so
//! nothing is left holding a client's keyboard when it closes; the keys are
//! simply intercepted in the filter, like a trusted prompt's. Pointer focus is
//! cleared while it is open and re-derived when it closes.
//!
//! **Dismissal.** Escape, a press outside the panel, a focus change away from
//! the window it belongs to, the session locking, a prompt or the region
//! selector taking the seat, the window going away or being minimized, or
//! leaving `de`. The focus, lock and window conditions are checked by
//! [`validate`], which every layout pass runs, so no path that changes them
//! has to know about the menu. Under the session lock it never opens.
//!
//! Keyboard: Up/Down (Home/End) move the highlight over the enabled rows, Enter
//! or Space activates, Right opens "Move to desktop", Left goes back.

use smithay::{
    desktop::Window,
    input::keyboard::Keysym,
    input::pointer::MotionEvent,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
};

use ec_abyss_render::menu::{place, MenuRow, MenuView};

use crate::state::AbyssState;

use super::{focus, maximize, titlebar, winops, workspace};

const BTN_LEFT: u32 = 0x110;

/// A menu entry's meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Minimize,
    Maximize,
    Shade,
    KeepAbove,
    KeepBelow,
    Sticky,
    /// Opens the desktop list.
    MoveTo,
    /// Workspace `n`, 0-based.
    Desktop(usize),
    /// Back from the desktop list.
    Back,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Main,
    Desktops,
}

/// What the rows depend on, read off the window when the menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    pub maximized: bool,
    pub shaded: bool,
    /// A bar to roll up to and not fullscreen.
    pub can_shade: bool,
    pub above: bool,
    pub below: bool,
    pub sticky: bool,
    /// The workspace the window is on, 0-based.
    pub workspace: usize,
}

/// What a key or click came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Nothing,
    Close,
    Do(Item),
}

/// The menu without a compositor around it: rows, highlight, paging, keys.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuModel {
    page: Page,
    items: Vec<Item>,
    view: MenuView,
    flags: Flags,
    anchor: Point<i32, Logical>,
    area: Rectangle<i32, Logical>,
}

fn row(label: &str, checked: bool, enabled: bool) -> MenuRow {
    MenuRow {
        checked,
        enabled,
        ..MenuRow::new(label)
    }
}

impl MenuModel {
    pub fn new(flags: Flags, anchor: Point<i32, Logical>, area: Rectangle<i32, Logical>) -> MenuModel {
        let mut m = MenuModel {
            page: Page::Main,
            items: Vec::new(),
            view: MenuView::default(),
            flags,
            anchor,
            area,
        };
        m.build(Page::Main);
        m
    }

    fn build(&mut self, page: Page) {
        let f = self.flags;
        let mut items = Vec::new();
        let mut rows = Vec::new();
        let mut add = |item: Item, r: MenuRow| {
            items.push(item);
            rows.push(r);
        };
        match page {
            Page::Main => {
                add(Item::Minimize, row("Minimize", false, true));
                add(
                    Item::Maximize,
                    row(if f.maximized { "Restore" } else { "Maximize" }, false, true),
                );
                add(
                    Item::Shade,
                    row(
                        if f.shaded { "Unshade" } else { "Shade" },
                        false,
                        f.shaded || f.can_shade,
                    ),
                );
                add(Item::KeepAbove, row("Keep above", f.above, true));
                add(Item::KeepBelow, row("Keep below", f.below, true));
                add(Item::Sticky, row("On all desktops", f.sticky, true));
                add(
                    Item::MoveTo,
                    MenuRow {
                        submenu: true,
                        ..row("Move to desktop", false, true)
                    },
                );
                add(
                    Item::Close,
                    MenuRow {
                        separator_before: true,
                        ..row("Close", false, true)
                    },
                );
            }
            Page::Desktops => {
                add(Item::Back, row("Back", false, true));
                for n in 0..workspace::COUNT {
                    // The window's own desktop is where it already is.
                    let here = n == f.workspace && !f.sticky;
                    add(Item::Desktop(n), row(&format!("Desktop {}", n + 1), here, !here));
                }
            }
        }
        self.page = page;
        self.items = items;
        self.view.rows = rows;
        self.view.hover = None;
        self.view.origin = place(self.anchor, self.view.size(), self.area);
    }

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn view(&self) -> &MenuView {
        &self.view
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn hover(&self) -> Option<usize> {
        self.view.hover
    }

    fn enabled(&self, i: usize) -> bool {
        self.view.rows.get(i).is_some_and(|r| r.enabled)
    }

    /// Light the row under `pos`. True when the highlight moved.
    pub fn hover_at(&mut self, pos: Point<f64, Logical>) -> bool {
        let next = self.view.row_at(pos).filter(|&i| self.enabled(i));
        let changed = next != self.view.hover;
        self.view.hover = next;
        changed
    }

    /// Move the highlight `delta` enabled rows (wraps). From no highlight, down
    /// goes to the first enabled row and up to the last.
    pub fn step(&mut self, delta: i32) {
        let n = self.items.len();
        if n == 0 || !(0..n).any(|i| self.enabled(i)) {
            return;
        }
        let mut at = match self.view.hover {
            Some(i) => i as i32,
            None if delta > 0 => -1,
            None => n as i32,
        };
        loop {
            at = (at + delta).rem_euclid(n as i32);
            if self.enabled(at as usize) {
                break;
            }
        }
        self.view.hover = Some(at as usize);
    }

    fn edge(&mut self, last: bool) {
        self.view.hover = None;
        self.step(if last { -1 } else { 1 });
    }

    /// Activate the highlighted row.
    pub fn activate(&mut self) -> Act {
        let Some(i) = self.view.hover.filter(|&i| self.enabled(i)) else {
            return Act::Nothing;
        };
        match self.items[i] {
            Item::MoveTo => {
                self.build(Page::Desktops);
                // Land on the first desktop that can be chosen.
                self.view.hover = Some(0);
                self.step(1);
                Act::Nothing
            }
            Item::Back => {
                self.build(Page::Main);
                Act::Nothing
            }
            item => Act::Do(item),
        }
    }

    /// One key press (after case folding).
    pub fn key(&mut self, sym: Keysym) -> Act {
        match sym {
            Keysym::Escape => Act::Close,
            Keysym::Down => {
                self.step(1);
                Act::Nothing
            }
            Keysym::Up => {
                self.step(-1);
                Act::Nothing
            }
            Keysym::Home => {
                self.edge(false);
                Act::Nothing
            }
            Keysym::End => {
                self.edge(true);
                Act::Nothing
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => self.activate(),
            Keysym::Right => {
                if self.page == Page::Main && self.view.hover.map(|i| self.items[i]) == Some(Item::MoveTo) {
                    self.activate()
                } else {
                    Act::Nothing
                }
            }
            Keysym::Left => {
                if self.page == Page::Desktops {
                    self.build(Page::Main);
                }
                Act::Nothing
            }
            _ => Act::Nothing,
        }
    }
}

#[derive(Debug)]
struct Open {
    window: Window,
    model: MenuModel,
}

/// The open menu, if any. Plain data owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct WinMenu {
    open: Option<Open>,
}

impl WinMenu {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The window the open menu belongs to.
    pub fn window(&self) -> Option<&Window> {
        self.open.as_ref().map(|o| &o.window)
    }

    /// What the renderer draws.
    pub fn view(&self) -> Option<&MenuView> {
        self.open.as_ref().map(|o| o.model.view())
    }

    pub fn model(&self) -> Option<&MenuModel> {
        self.open.as_ref().map(|o| &o.model)
    }

    /// Drop the menu without touching anything else.
    pub fn close(&mut self) {
        self.open = None;
    }
}

fn flags_of(state: &AbyssState, window: &Window) -> Flags {
    let ws = state
        .outputs
        .iter()
        .find_map(|e| e.workspaces.iter().position(|ws| ws.holds(window)))
        .unwrap_or(0);
    let band = state.winops.band(window);
    Flags {
        maximized: maximize::is_maximized(state, window),
        shaded: state.winops.is_shaded(window),
        can_shade: titlebar::bar_height(state, window) > 0 && !state.fullscreen.contains_key(window),
        above: band == winops::Band::Above,
        below: band == winops::Band::Below,
        sticky: state.winops.is_sticky(window),
        workspace: ws,
    }
}

/// May a menu exist right now? `de`, a live session, nothing else holding the
/// human's seat.
fn allowed(state: &AbyssState) -> bool {
    state.config.mode.profile().titlebars
        && !state.lock.locked
        && !crate::trusted_ui::holds_seat(state)
        && !state.region_select.active()
}

/// Open the window menu for `window` with its corner at `anchor` (global
/// logical), moved to stay on the output. Focuses and raises the window first
/// (the menu is the human's act on it). False, and nothing opens, when the
/// session is locked, the mode is not `de`, a prompt or the region selector
/// holds the seat, the window is not placed on an output or is minimized, or a
/// move or resize is under way.
///
/// For human input only: the titlebar's right press, `show_window_menu` with a
/// live press serial, and the M6 socket method on the human socket.
pub fn open_window_menu(state: &mut AbyssState, window: &Window, anchor: Point<i32, Logical>) -> bool {
    if !allowed(state)
        || super::is_unmanaged_x11(state, window)
        || super::is_minimized(state, window)
        || super::output_of_window(state, window).is_none()
        || crate::input::grabs::drag_active(state)
    {
        return false;
    }
    state.winmenu.close();
    focus::focus_window_raising(state, window, true);
    if state.focus.as_ref() != Some(window) {
        return false;
    }
    let area = super::output_at(state, anchor.to_f64()).map_or_else(
        || Rectangle::new(Point::from((0, 0)), (1, 1).into()),
        |o| super::usable_area(state, &o),
    );
    let model = MenuModel::new(flags_of(state, window), anchor, area);
    // The client under the pointer is not pointed at while the menu is up.
    if let Some(pointer) = state.seat.get_pointer() {
        let time = state.start_time.elapsed().as_millis() as u32;
        pointer.motion(
            state,
            None,
            &MotionEvent {
                location: state.pointer_location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(state);
    }
    state.last_pointer_focus = None;
    state.winmenu.open = Some(Open {
        window: window.clone(),
        model,
    });
    tracing::info!("window menu opened");
    crate::backend::damage_all(state);
    true
}

/// Close the menu if what it stands on is gone: the session locked, the mode
/// left, a prompt or the selector took the seat, focus moved off its window,
/// or the window went away or was minimized. Run at the top of every layout
/// pass. Never re-enters focus.
pub(super) fn validate(state: &mut AbyssState) {
    let Some(open) = &state.winmenu.open else {
        return;
    };
    let stale = !allowed(state)
        || state.focus.as_ref() != Some(&open.window)
        || super::is_minimized(state, &open.window)
        || super::output_of_window(state, &open.window).is_none();
    if stale {
        tracing::info!("window menu dismissed: what it stood on changed");
        state.winmenu.close();
        crate::backend::damage_all(state);
    }
}

/// Close from an input path, then let the pointer find what is under it again.
fn dismiss(state: &mut AbyssState) {
    tracing::info!("window menu closed");
    state.winmenu.close();
    crate::backend::damage_all(state);
    state.refresh_pointer_focus();
}

/// A key while the menu is open (the input filter has already taken it from
/// every client and binding).
pub fn key(state: &mut AbyssState, sym: Keysym) {
    let Some(open) = &mut state.winmenu.open else {
        return;
    };
    let before = open.model.clone();
    let act = open.model.key(sym);
    let window = open.window.clone();
    let moved = open.model != before;
    match act {
        Act::Nothing => {
            if moved {
                crate::backend::damage_all(state);
            }
        }
        Act::Close => dismiss(state),
        Act::Do(item) => {
            dismiss(state);
            run(state, &window, item);
        }
    }
}

/// The pointer moved while the menu is open.
pub fn motion(state: &mut AbyssState, pos: Point<f64, Logical>) {
    let Some(open) = &mut state.winmenu.open else {
        return;
    };
    if open.model.hover_at(pos) {
        crate::backend::damage_all(state);
    }
}

/// A pointer button while the menu is open. Always consumed: the menu holds
/// the pointer. A press outside the panel dismisses it (and goes no further);
/// the left button on a row activates it. The press is recorded so its release
/// is eaten too (`titlebar::release`).
pub fn button(state: &mut AbyssState, button: u32, pressed: bool) {
    let pos = state.pointer_location;
    let Some(open) = &mut state.winmenu.open else {
        return;
    };
    if !pressed {
        return;
    }
    state.bar_ui.swallow(button);
    if !open.model.view().contains(pos) {
        dismiss(state);
        return;
    }
    if button != BTN_LEFT {
        return;
    }
    open.model.hover_at(pos);
    let window = open.window.clone();
    match open.model.activate() {
        Act::Nothing => crate::backend::damage_all(state),
        Act::Close => dismiss(state),
        Act::Do(item) => {
            dismiss(state);
            run(state, &window, item);
        }
    }
}

/// Do what a row says, to `window`.
fn run(state: &mut AbyssState, window: &Window, item: Item) {
    match item {
        Item::Minimize => super::minimize_window(state, window),
        Item::Maximize => maximize::toggle_window(state, window),
        Item::Shade => {
            winops::toggle_shaded(state, window);
        }
        Item::KeepAbove => {
            winops::toggle_keep_above(state, window);
        }
        Item::KeepBelow => {
            winops::toggle_keep_below(state, window);
        }
        Item::Sticky => {
            winops::toggle_sticky(state, window);
        }
        Item::Desktop(n) => {
            // Choosing a desktop for a sticky window ends its stickiness: it
            // lives on that desktop now.
            winops::set_sticky(state, window, false);
            super::move_window_to_workspace(state, window, n + 1);
        }
        Item::Close => super::close_window(state, window),
        Item::MoveTo | Item::Back => {}
    }
}

/// `xdg_toplevel.show_window_menu`: honoured only for a serial of a press the
/// requesting client was really sent and that is still held, like a client
/// move. `location` is surface-local.
pub fn show_for_client(
    state: &mut AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
    serial: smithay::utils::Serial,
    location: Point<i32, Logical>,
) {
    let wl = surface.wl_surface().clone();
    if crate::input::grabs::start_data(state, &wl, serial).is_none() {
        return;
    }
    let Some(window) = super::window_for_surface(state, &wl) else {
        return;
    };
    let Some(content) = state.space.element_geometry(&window) else {
        return;
    };
    // The client's press holds an implicit grab; the menu replaces it.
    if let Some(pointer) = state.seat.get_pointer() {
        if pointer.is_grabbed() && !state.pointer_grab_active {
            let time = state.start_time.elapsed().as_millis() as u32;
            pointer.unset_grab(state, SERIAL_COUNTER.next_serial(), time);
        }
    }
    open_window_menu(state, &window, content.loc + location);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> Flags {
        Flags {
            maximized: false,
            shaded: false,
            can_shade: true,
            above: false,
            below: false,
            sticky: false,
            workspace: 2,
        }
    }

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((0, 0)), (1280, 800).into())
    }

    fn model() -> MenuModel {
        MenuModel::new(flags(), Point::from((100, 100)), area())
    }

    fn item_at(m: &MenuModel) -> Option<Item> {
        m.hover().map(|i| m.items()[i])
    }

    #[test]
    fn the_main_page_lists_the_operations_in_order() {
        let m = model();
        let labels: Vec<&str> = m.view().rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Minimize",
                "Maximize",
                "Shade",
                "Keep above",
                "Keep below",
                "On all desktops",
                "Move to desktop",
                "Close"
            ]
        );
        assert_eq!(m.page(), Page::Main);
        assert_eq!(m.hover(), None, "nothing is lit until the human moves");
    }

    #[test]
    fn labels_and_checks_follow_the_window() {
        let f = Flags {
            maximized: true,
            shaded: true,
            above: true,
            sticky: true,
            ..flags()
        };
        let m = MenuModel::new(f, Point::from((0, 0)), area());
        let rows = &m.view().rows;
        assert_eq!(rows[1].label, "Restore");
        assert_eq!(rows[2].label, "Unshade");
        assert!(rows[3].checked && !rows[4].checked && rows[5].checked);
    }

    #[test]
    fn shade_is_off_for_a_window_with_no_bar_unless_it_is_already_shaded() {
        let f = Flags {
            can_shade: false,
            ..flags()
        };
        let m = MenuModel::new(f, Point::from((0, 0)), area());
        assert!(!m.view().rows[2].enabled);
        let m = MenuModel::new(
            Flags {
                shaded: true,
                can_shade: false,
                ..flags()
            },
            Point::from((0, 0)),
            area(),
        );
        assert!(m.view().rows[2].enabled, "it can always be rolled back down");
    }

    #[test]
    fn arrows_walk_the_enabled_rows_and_wrap() {
        let mut m = MenuModel::new(
            Flags {
                can_shade: false,
                ..flags()
            },
            Point::from((0, 0)),
            area(),
        );
        m.key(Keysym::Down);
        assert_eq!(
            item_at(&m),
            Some(Item::Minimize),
            "down from nothing is the first row"
        );
        m.key(Keysym::Down);
        assert_eq!(item_at(&m), Some(Item::Maximize));
        m.key(Keysym::Down);
        assert_eq!(
            item_at(&m),
            Some(Item::KeepAbove),
            "the disabled Shade row is skipped"
        );
        m.key(Keysym::Up);
        m.key(Keysym::Up);
        m.key(Keysym::Up);
        assert_eq!(
            item_at(&m),
            Some(Item::Close),
            "up from the first row wraps to the last"
        );
        m.key(Keysym::Home);
        assert_eq!(item_at(&m), Some(Item::Minimize));
        m.key(Keysym::End);
        assert_eq!(item_at(&m), Some(Item::Close));
    }

    #[test]
    fn up_from_nothing_goes_to_the_last_row() {
        let mut m = model();
        m.key(Keysym::Up);
        assert_eq!(item_at(&m), Some(Item::Close));
    }

    #[test]
    fn enter_activates_the_lit_row_and_nothing_else() {
        let mut m = model();
        assert_eq!(m.key(Keysym::Return), Act::Nothing, "no row is lit");
        m.key(Keysym::Down);
        assert_eq!(m.key(Keysym::Return), Act::Do(Item::Minimize));
        assert_eq!(m.key(Keysym::KP_Enter), Act::Do(Item::Minimize));
        assert_eq!(m.key(Keysym::space), Act::Do(Item::Minimize));
    }

    #[test]
    fn escape_dismisses() {
        let mut m = model();
        m.key(Keysym::Down);
        assert_eq!(m.key(Keysym::Escape), Act::Close);
    }

    #[test]
    fn other_keys_do_nothing() {
        let mut m = model();
        m.key(Keysym::Down);
        let before = m.clone();
        assert_eq!(m.key(Keysym::a), Act::Nothing);
        assert_eq!(m.key(Keysym::Tab), Act::Nothing);
        assert_eq!(m, before);
    }

    #[test]
    fn move_to_desktop_opens_the_workspace_list_and_back_returns() {
        let mut m = model();
        m.key(Keysym::End);
        m.key(Keysym::Up);
        assert_eq!(item_at(&m), Some(Item::MoveTo));
        assert_eq!(m.key(Keysym::Return), Act::Nothing, "it opens a page");
        assert_eq!(m.page(), Page::Desktops);
        assert_eq!(m.view().rows.len(), 1 + workspace::COUNT);
        assert_eq!(item_at(&m), Some(Item::Desktop(0)), "the first choice is lit");
        // The window's own desktop is shown checked and cannot be chosen.
        let own = m.items().iter().position(|i| *i == Item::Desktop(2)).unwrap();
        assert!(m.view().rows[own].checked && !m.view().rows[own].enabled);
        m.key(Keysym::Down);
        m.key(Keysym::Down);
        assert_eq!(item_at(&m), Some(Item::Desktop(3)), "desktop 3 is skipped");
        assert_eq!(m.key(Keysym::Return), Act::Do(Item::Desktop(3)));
        m.key(Keysym::Left);
        assert_eq!(m.page(), Page::Main);
    }

    #[test]
    fn right_arrow_opens_the_list_only_on_move_to_desktop() {
        let mut m = model();
        m.key(Keysym::Down);
        m.key(Keysym::Right);
        assert_eq!(m.page(), Page::Main);
        m.key(Keysym::End);
        m.key(Keysym::Up);
        m.key(Keysym::Right);
        assert_eq!(m.page(), Page::Desktops);
    }

    #[test]
    fn a_sticky_window_has_no_desktop_of_its_own() {
        let mut m = MenuModel::new(
            Flags {
                sticky: true,
                ..flags()
            },
            Point::from((0, 0)),
            area(),
        );
        m.key(Keysym::End);
        m.key(Keysym::Up);
        m.key(Keysym::Return);
        assert!(
            m.view().rows.iter().all(|r| r.enabled),
            "every desktop can be chosen"
        );
    }

    #[test]
    fn hover_follows_the_pointer_and_skips_disabled_rows() {
        let mut m = MenuModel::new(
            Flags {
                can_shade: false,
                ..flags()
            },
            Point::from((100, 100)),
            area(),
        );
        let o = m.view().origin;
        let at = |i: usize, m: &MenuModel| {
            let r = m.view().row_rect_local(i);
            Point::<f64, Logical>::from((f64::from(o.x + 10), f64::from(o.y + r.loc.y + 3)))
        };
        let p = at(1, &m);
        assert!(m.hover_at(p));
        assert_eq!(item_at(&m), Some(Item::Maximize));
        assert!(!m.hover_at(p), "the same row is no change");
        let p = at(2, &m);
        assert!(m.hover_at(p), "moving onto a disabled row clears the light");
        assert_eq!(m.hover(), None);
        assert!(m.hover_at(Point::from((0.0, 0.0))) || m.hover().is_none());
    }

    #[test]
    fn the_panel_is_placed_on_the_area_whatever_the_anchor() {
        let m = MenuModel::new(flags(), Point::from((1270, 790)), area());
        let r = m.view().rect();
        assert!(area().contains_rect(r), "{r:?} leaves the area");
    }
}
