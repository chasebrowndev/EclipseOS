// SPDX-License-Identifier: AGPL-3.0-only
//! The pointer controller (T-05 §5): decides, for every pointer event, whether the application
//! owns it (mouse reporting 1000/1002/1003 with 1006/1016) or the terminal does (selection,
//! wheel scrolling, middle-click paste). Pure: events in, [`Out`] actions out; the caller owns
//! the PTY, the seat and the render thread. Only cell positions and counts are ever logged.

use std::time::Instant;

use apoth_input::modes::bit;
use apoth_input::{
    InputModes, Mods, MouseButton, MouseEvent, MouseKind, MouseOutcome, MouseTracker, WheelAction,
    encode_mouse_outcome, encode_wheel_key, selection_override, wheel_action,
};

use crate::pointer::{ClickTracker, Geometry, Hit, button_of, kind_for};
use crate::settings::InputSettings;
use crate::view::{CopyTarget, ScrollBy, ViewCmd};

/// Most wheel notches turned into events by one pointer frame (a runaway axis never floods the
/// PTY).
const MAX_NOTCHES: i32 = 20;

/// What the caller does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Out {
    /// Bytes for the PTY, one write unit.
    Pty(Vec<u8>),
    /// A command for the render thread's view.
    View(ViewCmd),
    /// Paste the primary selection (middle click).
    PastePrimary,
}

#[derive(Debug, Default)]
pub(crate) struct Mouse {
    pos: Option<(f64, f64)>,
    click: ClickTracker,
    tracker: MouseTracker,
    /// A local selection drag is running.
    selecting: bool,
    /// The application was sent the press of this button (or suppressed it); the release
    /// goes the same way whatever the modes are by then.
    reported: Option<MouseButton>,
    /// A selection may exist (shift-click extends it).
    sel: bool,
}

impl Mouse {
    pub(crate) fn new() -> Mouse {
        Mouse::default()
    }

    pub(crate) fn left(&mut self) {
        self.pos = None;
        self.click.reset();
        self.tracker.reset();
    }

    /// The selection was dropped from outside (typing).
    pub(crate) fn selection_gone(&mut self) -> bool {
        self.selecting = false;
        std::mem::take(&mut self.sel)
    }

    /// A selection was made by other means (select all).
    pub(crate) fn mark_selection(&mut self) {
        self.sel = true;
    }

    fn event(hit: &Hit, button: Option<MouseButton>, kind: MouseKind, mods: Mods) -> MouseEvent {
        MouseEvent {
            button,
            kind,
            mods,
            col: hit.col as u32,
            row: hit.row as u32,
            px: hit.px.0,
            py: hit.px.1,
        }
    }

    /// Encode for the application; `None` when the mode does not report this event.
    fn report(ev: &MouseEvent, modes: InputModes) -> Option<Option<Vec<u8>>> {
        let mut buf = [0u8; 32];
        match encode_mouse_outcome(ev, modes, &mut buf) {
            MouseOutcome::Sent(n) => Some(Some(buf[..n].to_vec())),
            // The application asked for it but X10 cannot carry it: it still owns the mouse.
            MouseOutcome::Suppressed => Some(None),
            MouseOutcome::NotReported => None,
        }
    }

    fn captured(modes: InputModes, mods: Mods, cfg: &InputSettings) -> bool {
        modes.mouse_mode() != apoth_input::MouseMode::None
            && !selection_override(mods, cfg.override_mods)
    }

    /// Pointer motion at logical `(x, y)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn motion(
        &mut self,
        geo: &Geometry,
        x: f64,
        y: f64,
        held: Option<MouseButton>,
        mods: Mods,
        modes: InputModes,
        cfg: &InputSettings,
    ) -> Vec<Out> {
        self.pos = Some((x, y));
        let hit = geo.hit(x, y);
        let mut out = Vec::new();
        if self.selecting {
            Self::drag_to(&hit, &mut out);
            return out;
        }
        // An application that owns a button press owns the drag that follows it.
        let button = self.reported.or(held);
        if self.reported.is_some() || Self::captured(modes, mods, cfg) {
            let ev = Self::event(&hit, button, MouseKind::Motion, mods);
            if self.tracker.motion_changed(&ev, modes)
                && let Some(Some(b)) = Self::report(&ev, modes)
            {
                out.push(Out::Pty(b));
            }
        }
        out
    }

    /// Extend a drag to `hit`, scrolling one line when it is past an edge.
    fn drag_to(hit: &Hit, out: &mut Vec<Out>) {
        if hit.above {
            out.push(Out::View(ViewCmd::Scroll(ScrollBy::Lines(1))));
        } else if hit.below {
            out.push(Out::View(ViewCmd::Scroll(ScrollBy::Lines(-1))));
        }
        out.push(Out::View(ViewCmd::Extend {
            row: hit.row,
            col: hit.col,
            side: hit.side,
        }));
    }

    /// A drag is held with the pointer past the top or bottom edge: the caller runs the
    /// autoscroll timer exactly while this is true (no timer when idle, T-23 IDLE).
    pub(crate) fn autoscroll_wanted(&self, geo: &Geometry) -> bool {
        self.selecting
            && self.pos.is_some_and(|(x, y)| {
                let h = geo.hit(x, y);
                h.above || h.below
            })
    }

    /// One autoscroll tick: the same step a motion past the edge makes.
    pub(crate) fn autoscroll_tick(&mut self, geo: &Geometry) -> Vec<Out> {
        let mut out = Vec::new();
        if self.autoscroll_wanted(geo)
            && let Some((x, y)) = self.pos
        {
            Self::drag_to(&geo.hit(x, y), &mut out);
        }
        out
    }

    /// A button at the last known position (`code` is the evdev code).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn button(
        &mut self,
        geo: &Geometry,
        code: u32,
        pressed: bool,
        mods: Mods,
        modes: InputModes,
        now: Instant,
        cfg: &InputSettings,
    ) -> Vec<Out> {
        let Some(btn) = button_of(code) else {
            return Vec::new();
        };
        let (x, y) = self.pos.unwrap_or((0.0, 0.0));
        let hit = geo.hit(x, y);
        let mut out = Vec::new();
        self.tracker.reset();

        if !pressed {
            if self.reported == Some(btn) {
                self.reported = None;
                // The release goes to the application only while it still asks for events.
                let ev = Self::event(&hit, Some(btn), MouseKind::Release, mods);
                if let Some(Some(b)) = Self::report(&ev, modes) {
                    out.push(Out::Pty(b));
                }
                return out;
            }
            if matches!(btn, MouseButton::Left | MouseButton::Right) && self.selecting {
                self.selecting = false;
                // `selection.copy-on-select`: none, primary, clipboard or both (T-05 §5).
                if cfg.copy_on_select.primary() && cfg.primary {
                    out.push(Out::View(ViewCmd::Finish {
                        copy: Some(CopyTarget::Primary),
                    }));
                }
                if cfg.copy_on_select.clipboard() {
                    out.push(Out::View(ViewCmd::Finish {
                        copy: Some(CopyTarget::Clipboard),
                    }));
                }
            }
            return out;
        }

        if Self::captured(modes, mods, cfg) {
            let ev = Self::event(&hit, Some(btn), MouseKind::Press, mods);
            if let Some(sent) = Self::report(&ev, modes) {
                self.reported = Some(btn);
                if let Some(b) = sent {
                    out.push(Out::Pty(b));
                }
                return out;
            }
        }

        match btn {
            MouseButton::Left => {
                let count = self.click.press(now, (hit.col, hit.row), cfg.multi_click);
                let extend = mods.contains(Mods::SHIFT) && self.sel && count == 1;
                if extend {
                    out.push(Out::View(ViewCmd::Extend {
                        row: hit.row,
                        col: hit.col,
                        side: hit.side,
                    }));
                } else {
                    out.push(Out::View(ViewCmd::Begin {
                        kind: kind_for(count, mods, cfg.triple_line),
                        row: hit.row,
                        col: hit.col,
                        side: hit.side,
                    }));
                    self.sel = true;
                }
                self.selecting = true;
            }
            // Right click extends the selection (T-05 §5, INPUT-36) and a drag keeps extending.
            MouseButton::Right if self.sel => {
                out.push(Out::View(ViewCmd::Extend {
                    row: hit.row,
                    col: hit.col,
                    side: hit.side,
                }));
                self.selecting = true;
            }
            MouseButton::Middle if cfg.primary => out.push(Out::PastePrimary),
            _ => {}
        }
        out
    }

    /// One pointer frame of scrolling: whole lines, positive is down.
    pub(crate) fn scroll(
        &mut self,
        geo: &Geometry,
        lines_y: i32,
        lines_x: i32,
        mods: Mods,
        modes: InputModes,
        cfg: &InputSettings,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        let (x, y) = self.pos.unwrap_or((0.0, 0.0));
        let hit = geo.hit(x, y);
        let up = lines_y < 0;
        let n = lines_y.abs().min(MAX_NOTCHES);
        if n > 0 {
            let action = if Self::captured(modes, mods, cfg) || !is_reporting(modes) {
                wheel_action(modes)
            } else {
                // Override held over a capturing application: scroll the terminal itself.
                WheelAction::Viewport
            };
            match action {
                WheelAction::Report => {
                    let b = if up {
                        MouseButton::WheelUp
                    } else {
                        MouseButton::WheelDown
                    };
                    let ev = Self::event(&hit, Some(b), MouseKind::Press, mods);
                    if let Some(Some(bytes)) = Self::report(&ev, modes) {
                        for _ in 0..n {
                            out.push(Out::Pty(bytes.clone()));
                        }
                    }
                }
                WheelAction::AltScrollKeys => {
                    let mut buf = [0u8; 64];
                    let k = encode_wheel_key(up, Mods::NONE, modes, &mut buf);
                    if k > 0 {
                        let mut bytes = Vec::new();
                        for _ in 0..n * cfg.scroll_lines as i32 {
                            bytes.extend_from_slice(&buf[..k]);
                        }
                        out.push(Out::Pty(bytes));
                    }
                }
                WheelAction::Viewport => {
                    let lines = (f64::from(n) * f64::from(cfg.scroll_lines) * cfg.scroll_multiplier)
                        .round()
                        .max(1.0) as i32;
                    out.push(Out::View(ViewCmd::Scroll(ScrollBy::Lines(if up {
                        lines
                    } else {
                        -lines
                    }))));
                }
            }
        }
        let nx = lines_x.abs().min(MAX_NOTCHES);
        if nx > 0 && Self::captured(modes, mods, cfg) {
            let b = if lines_x < 0 {
                MouseButton::WheelLeft
            } else {
                MouseButton::WheelRight
            };
            let ev = Self::event(&hit, Some(b), MouseKind::Press, mods);
            if let Some(Some(bytes)) = Self::report(&ev, modes) {
                for _ in 0..nx {
                    out.push(Out::Pty(bytes.clone()));
                }
            }
        }
        out
    }
}

fn is_reporting(modes: InputModes) -> bool {
    modes.has(bit::MOUSE_1000) || modes.has(bit::MOUSE_1002) || modes.has(bit::MOUSE_1003)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{CopyOnSelect, Guard, PasteSettings};
    use apoth_grid::{SelKind, Side};
    use apoth_input::PasteOptions;
    use std::time::Duration;

    fn cfg() -> InputSettings {
        InputSettings {
            copy_on_select: CopyOnSelect::Primary,
            primary: true,
            clear_on_copy: false,
            clear_on_typing: true,
            trim_trailing: true,
            word_extra: String::new(),
            triple_line: true,
            multi_click: Duration::from_millis(400),
            scroll_lines: 3,
            scroll_multiplier: 1.0,
            override_mods: Mods::SHIFT,
            paste: PasteSettings {
                guard: Guard::Auto,
                confirm_bytes: 4096,
                confirm_multiline: true,
                opts: PasteOptions::default(),
            },
        }
    }

    fn geo() -> Geometry {
        Geometry {
            pad: (0, 0),
            cell: (10, 20),
            scale: 1.0,
            rows: 24,
            cols: 80,
        }
    }

    fn modes(bits: u64) -> InputModes {
        InputModes::from_raw(bits, 0)
    }

    const LEFT: u32 = 0x110;
    const MIDDLE: u32 = 0x112;
    const RIGHT: u32 = 0x111;

    fn at(m: &mut Mouse, x: f64, y: f64, mo: InputModes) {
        m.motion(&geo(), x, y, None, Mods::NONE, mo, &cfg());
    }

    #[test]
    fn drag_selects_and_copies_to_primary_on_release() {
        let mut m = Mouse::new();
        let t = Instant::now();
        at(&mut m, 12.0, 5.0, modes(0));
        let o = m.button(&geo(), LEFT, true, Mods::NONE, modes(0), t, &cfg());
        assert_eq!(
            o,
            vec![Out::View(ViewCmd::Begin {
                kind: SelKind::Simple,
                row: 0,
                col: 1,
                side: Side::Left
            })]
        );
        let o = m.motion(
            &geo(),
            55.0,
            25.0,
            Some(MouseButton::Left),
            Mods::NONE,
            modes(0),
            &cfg(),
        );
        assert_eq!(
            o,
            vec![Out::View(ViewCmd::Extend {
                row: 1,
                col: 5,
                side: Side::Right
            })]
        );
        let o = m.button(&geo(), LEFT, false, Mods::NONE, modes(0), t, &cfg());
        assert_eq!(
            o,
            vec![Out::View(ViewCmd::Finish {
                copy: Some(CopyTarget::Primary)
            })]
        );
    }

    #[test]
    fn copy_on_select_targets() {
        for (mode, want) in [
            (CopyOnSelect::Primary, vec![CopyTarget::Primary]),
            (CopyOnSelect::Clipboard, vec![CopyTarget::Clipboard]),
            (
                CopyOnSelect::Both,
                vec![CopyTarget::Primary, CopyTarget::Clipboard],
            ),
        ] {
            let mut c = cfg();
            c.copy_on_select = mode;
            let mut m = Mouse::new();
            m.motion(&geo(), 1.0, 1.0, None, Mods::NONE, modes(0), &c);
            m.button(&geo(), LEFT, true, Mods::NONE, modes(0), Instant::now(), &c);
            let o = m.button(
                &geo(),
                LEFT,
                false,
                Mods::NONE,
                modes(0),
                Instant::now(),
                &c,
            );
            let got: Vec<CopyTarget> = o
                .iter()
                .filter_map(|o| match o {
                    Out::View(ViewCmd::Finish { copy: Some(t) }) => Some(*t),
                    _ => None,
                })
                .collect();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn held_drag_past_the_edge_ticks_until_it_returns() {
        let mut m = Mouse::new();
        let c = cfg();
        // geo(): 24 rows of 20 px = 480 px tall.
        m.motion(&geo(), 5.0, 5.0, None, Mods::NONE, modes(0), &c);
        assert!(!m.autoscroll_wanted(&geo()), "no drag, no timer");
        m.button(&geo(), LEFT, true, Mods::NONE, modes(0), Instant::now(), &c);
        assert!(!m.autoscroll_wanted(&geo()), "inside the area, no timer");
        m.motion(
            &geo(),
            5.0,
            600.0,
            Some(MouseButton::Left),
            Mods::NONE,
            modes(0),
            &c,
        );
        assert!(m.autoscroll_wanted(&geo()));
        // The pointer does not move: each tick still scrolls (older rows are positive).
        for _ in 0..3 {
            let o = m.autoscroll_tick(&geo());
            assert_eq!(o[0], Out::View(ViewCmd::Scroll(ScrollBy::Lines(-1))));
            assert!(matches!(o[1], Out::View(ViewCmd::Extend { row: 23, .. })));
        }
        m.motion(
            &geo(),
            5.0,
            -10.0,
            Some(MouseButton::Left),
            Mods::NONE,
            modes(0),
            &c,
        );
        let o = m.autoscroll_tick(&geo());
        assert_eq!(o[0], Out::View(ViewCmd::Scroll(ScrollBy::Lines(1))));
        // Back inside, or released: nothing ticks.
        m.motion(
            &geo(),
            5.0,
            100.0,
            Some(MouseButton::Left),
            Mods::NONE,
            modes(0),
            &c,
        );
        assert!(!m.autoscroll_wanted(&geo()) && m.autoscroll_tick(&geo()).is_empty());
        m.motion(
            &geo(),
            5.0,
            600.0,
            Some(MouseButton::Left),
            Mods::NONE,
            modes(0),
            &c,
        );
        m.button(
            &geo(),
            LEFT,
            false,
            Mods::NONE,
            modes(0),
            Instant::now(),
            &c,
        );
        assert!(!m.autoscroll_wanted(&geo()));
    }

    #[test]
    fn right_click_extends_an_existing_selection_only() {
        let mut m = Mouse::new();
        let c = cfg();
        let t = Instant::now();
        at(&mut m, 12.0, 5.0, modes(0));
        // No selection yet: a right click does nothing.
        assert!(
            m.button(&geo(), RIGHT, true, Mods::NONE, modes(0), t, &c)
                .is_empty()
        );
        m.button(&geo(), LEFT, true, Mods::NONE, modes(0), t, &c);
        m.button(&geo(), LEFT, false, Mods::NONE, modes(0), t, &c);
        at(&mut m, 55.0, 25.0, modes(0));
        let o = m.button(&geo(), RIGHT, true, Mods::NONE, modes(0), t, &c);
        assert_eq!(
            o,
            vec![Out::View(ViewCmd::Extend {
                row: 1,
                col: 5,
                side: Side::Right
            })]
        );
        // The drag that follows keeps extending, and the release copies.
        let o = m.motion(&geo(), 95.0, 25.0, None, Mods::NONE, modes(0), &c);
        assert!(matches!(o[0], Out::View(ViewCmd::Extend { col: 9, .. })));
        let o = m.button(&geo(), RIGHT, false, Mods::NONE, modes(0), t, &c);
        assert_eq!(
            o,
            vec![Out::View(ViewCmd::Finish {
                copy: Some(CopyTarget::Primary)
            })]
        );
    }

    #[test]
    fn release_copies_nothing_without_copy_on_select() {
        let mut c = cfg();
        c.copy_on_select = CopyOnSelect::None;
        let mut m = Mouse::new();
        m.motion(&geo(), 1.0, 1.0, None, Mods::NONE, modes(0), &c);
        m.button(&geo(), LEFT, true, Mods::NONE, modes(0), Instant::now(), &c);
        let o = m.button(
            &geo(),
            LEFT,
            false,
            Mods::NONE,
            modes(0),
            Instant::now(),
            &c,
        );
        assert!(o.is_empty());
    }

    #[test]
    fn double_and_triple_click_pick_word_and_line_and_alt_is_block() {
        let mut m = Mouse::new();
        let t = Instant::now();
        at(&mut m, 12.0, 5.0, modes(0));
        let kind = |o: &[Out]| match &o[0] {
            Out::View(ViewCmd::Begin { kind, .. }) => *kind,
            other => panic!("{other:?}"),
        };
        let c = cfg();
        let o = m.button(&geo(), LEFT, true, Mods::NONE, modes(0), t, &c);
        assert_eq!(kind(&o), SelKind::Simple);
        m.button(&geo(), LEFT, false, Mods::NONE, modes(0), t, &c);
        let t2 = t + Duration::from_millis(100);
        let o = m.button(&geo(), LEFT, true, Mods::NONE, modes(0), t2, &c);
        assert_eq!(kind(&o), SelKind::Word);
        m.button(&geo(), LEFT, false, Mods::NONE, modes(0), t2, &c);
        let t3 = t + Duration::from_millis(200);
        let o = m.button(&geo(), LEFT, true, Mods::NONE, modes(0), t3, &c);
        assert_eq!(kind(&o), SelKind::Line);
        m.button(&geo(), LEFT, false, Mods::NONE, modes(0), t3, &c);
        let t4 = t + Duration::from_secs(5);
        let o = m.button(&geo(), LEFT, true, Mods::ALT, modes(0), t4, &c);
        assert_eq!(kind(&o), SelKind::Block);
    }

    #[test]
    fn shift_click_extends_an_existing_selection() {
        let mut m = Mouse::new();
        let t = Instant::now();
        let c = cfg();
        at(&mut m, 12.0, 5.0, modes(0));
        m.button(&geo(), LEFT, true, Mods::NONE, modes(0), t, &c);
        m.button(&geo(), LEFT, false, Mods::NONE, modes(0), t, &c);
        at(&mut m, 200.0, 45.0, modes(0));
        let o = m.button(
            &geo(),
            LEFT,
            true,
            Mods::SHIFT,
            modes(0),
            t + Duration::from_secs(3),
            &c,
        );
        assert!(matches!(
            o[0],
            Out::View(ViewCmd::Extend {
                row: 2,
                col: 20,
                ..
            })
        ));
        m.selection_gone();
        m.button(&geo(), LEFT, false, Mods::SHIFT, modes(0), t, &c);
        let o = m.button(
            &geo(),
            LEFT,
            true,
            Mods::SHIFT,
            modes(0),
            t + Duration::from_secs(9),
            &c,
        );
        assert!(matches!(o[0], Out::View(ViewCmd::Begin { .. })));
    }

    #[test]
    fn drag_past_the_edge_autoscrolls() {
        let mut m = Mouse::new();
        let c = cfg();
        at(&mut m, 12.0, 100.0, modes(0));
        m.button(&geo(), LEFT, true, Mods::NONE, modes(0), Instant::now(), &c);
        let o = m.motion(
            &geo(),
            12.0,
            9000.0,
            Some(MouseButton::Left),
            Mods::NONE,
            modes(0),
            &c,
        );
        assert_eq!(o[0], Out::View(ViewCmd::Scroll(ScrollBy::Lines(-1))));
        assert!(matches!(o[1], Out::View(ViewCmd::Extend { row: 23, .. })));
    }

    #[test]
    fn middle_click_pastes_primary() {
        let mut m = Mouse::new();
        at(&mut m, 1.0, 1.0, modes(0));
        let o = m.button(
            &geo(),
            MIDDLE,
            true,
            Mods::NONE,
            modes(0),
            Instant::now(),
            &cfg(),
        );
        assert_eq!(o, vec![Out::PastePrimary]);
    }

    #[test]
    fn reporting_modes_own_the_mouse() {
        let sgr = modes(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        let mut m = Mouse::new();
        at(&mut m, 25.0, 45.0, sgr);
        let t = Instant::now();
        let c = cfg();
        let o = m.button(&geo(), LEFT, true, Mods::NONE, sgr, t, &c);
        assert_eq!(o, vec![Out::Pty(b"\x1b[<0;3;3M".to_vec())]);
        let o = m.button(&geo(), LEFT, false, Mods::NONE, sgr, t, &c);
        assert_eq!(o, vec![Out::Pty(b"\x1b[<0;3;3m".to_vec())]);
        // Mode 1000 has no motion events.
        let o = m.motion(&geo(), 90.0, 45.0, None, Mods::NONE, sgr, &c);
        assert!(o.is_empty());
    }

    #[test]
    fn drag_mode_reports_motion_with_the_button_and_dedups_cells() {
        let m1002 = modes(bit::MOUSE_1002 | bit::MOUSE_SGR_1006);
        let mut m = Mouse::new();
        let c = cfg();
        at(&mut m, 5.0, 5.0, m1002);
        m.button(&geo(), LEFT, true, Mods::NONE, m1002, Instant::now(), &c);
        let o = m.motion(
            &geo(),
            25.0,
            5.0,
            Some(MouseButton::Left),
            Mods::NONE,
            m1002,
            &c,
        );
        assert_eq!(o, vec![Out::Pty(b"\x1b[<32;3;1M".to_vec())]);
        let again = m.motion(
            &geo(),
            26.0,
            6.0,
            Some(MouseButton::Left),
            Mods::NONE,
            m1002,
            &c,
        );
        assert!(again.is_empty());
    }

    #[test]
    fn pixel_mode_reports_pixels() {
        let px = modes(bit::MOUSE_1000 | bit::MOUSE_PIXELS_1016);
        let mut m = Mouse::new();
        at(&mut m, 25.0, 45.0, px);
        let o = m.button(&geo(), LEFT, true, Mods::NONE, px, Instant::now(), &cfg());
        assert_eq!(o, vec![Out::Pty(b"\x1b[<0;26;46M".to_vec())]);
    }

    #[test]
    fn shift_overrides_capture_for_selection() {
        let sgr = modes(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        let mut m = Mouse::new();
        at(&mut m, 25.0, 45.0, sgr);
        let o = m.button(&geo(), LEFT, true, Mods::SHIFT, sgr, Instant::now(), &cfg());
        assert!(matches!(o[0], Out::View(ViewCmd::Begin { .. })));
    }

    #[test]
    fn x10_overflow_is_swallowed_not_selected() {
        let x10 = modes(bit::MOUSE_1000);
        let mut m = Mouse::new();
        let g = Geometry { cols: 400, ..geo() };
        m.motion(&g, 3000.0, 5.0, None, Mods::NONE, x10, &cfg());
        let o = m.button(&g, LEFT, true, Mods::NONE, x10, Instant::now(), &cfg());
        assert!(o.is_empty());
    }

    #[test]
    fn wheel_scrolls_the_viewport_by_lines() {
        let mut m = Mouse::new();
        at(&mut m, 1.0, 1.0, modes(0));
        let o = m.scroll(&geo(), -1, 0, Mods::NONE, modes(0), &cfg());
        assert_eq!(o, vec![Out::View(ViewCmd::Scroll(ScrollBy::Lines(3)))]);
        let o = m.scroll(&geo(), 2, 0, Mods::NONE, modes(0), &cfg());
        assert_eq!(o, vec![Out::View(ViewCmd::Scroll(ScrollBy::Lines(-6)))]);
        let mut c = cfg();
        c.scroll_multiplier = 2.0;
        let o = m.scroll(&geo(), -1, 0, Mods::NONE, modes(0), &c);
        assert_eq!(o, vec![Out::View(ViewCmd::Scroll(ScrollBy::Lines(6)))]);
    }

    #[test]
    fn wheel_is_reported_as_buttons_under_capture() {
        let sgr =
            modes(bit::MOUSE_1000 | bit::MOUSE_SGR_1006 | bit::ALT_SCREEN | bit::ALT_SCROLL_1007);
        let mut m = Mouse::new();
        at(&mut m, 5.0, 5.0, sgr);
        let o = m.scroll(&geo(), -2, 0, Mods::NONE, sgr, &cfg());
        assert_eq!(o, vec![Out::Pty(b"\x1b[<64;1;1M".to_vec()); 2]);
        let o = m.scroll(&geo(), 1, 0, Mods::NONE, sgr, &cfg());
        assert_eq!(o, vec![Out::Pty(b"\x1b[<65;1;1M".to_vec())]);
        // Shift held scrolls the terminal's own view instead.
        let o = m.scroll(&geo(), -1, 0, Mods::SHIFT, sgr, &cfg());
        assert_eq!(o, vec![Out::View(ViewCmd::Scroll(ScrollBy::Lines(3)))]);
    }

    #[test]
    fn alternate_scroll_sends_arrow_keys_only_on_the_alternate_screen() {
        let alt = modes(bit::ALT_SCREEN | bit::ALT_SCROLL_1007);
        let mut m = Mouse::new();
        at(&mut m, 5.0, 5.0, alt);
        let o = m.scroll(&geo(), -1, 0, Mods::NONE, alt, &cfg());
        assert_eq!(o, vec![Out::Pty(b"\x1b[A\x1b[A\x1b[A".to_vec())]);
        let appcursor = modes(bit::ALT_SCREEN | bit::ALT_SCROLL_1007 | bit::DECCKM);
        let o = m.scroll(&geo(), 1, 0, Mods::NONE, appcursor, &cfg());
        assert_eq!(o, vec![Out::Pty(b"\x1bOB\x1bOB\x1bOB".to_vec())]);
        // Alternate screen with 1007 off: nothing to scroll, the viewport moves (a no-op there).
        let no_alt_scroll = modes(bit::ALT_SCREEN);
        let o = m.scroll(&geo(), -1, 0, Mods::NONE, no_alt_scroll, &cfg());
        assert!(matches!(o[0], Out::View(ViewCmd::Scroll(_))));
    }

    #[test]
    fn runaway_wheel_is_capped() {
        let sgr = modes(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        let mut m = Mouse::new();
        at(&mut m, 5.0, 5.0, sgr);
        let o = m.scroll(&geo(), -5000, 0, Mods::NONE, sgr, &cfg());
        assert_eq!(o.len(), MAX_NOTCHES as usize);
    }

    #[test]
    fn release_goes_to_the_application_that_took_the_press() {
        let sgr = modes(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        let mut m = Mouse::new();
        at(&mut m, 5.0, 5.0, sgr);
        let c = cfg();
        m.button(&geo(), LEFT, true, Mods::NONE, sgr, Instant::now(), &c);
        // The application turned reporting off meanwhile: the release is not a selection end.
        let o = m.button(
            &geo(),
            LEFT,
            false,
            Mods::NONE,
            modes(0),
            Instant::now(),
            &c,
        );
        assert!(o.is_empty());
    }
}
