// SPDX-License-Identifier: Apache-2.0
//! Mouse reporting (X10 `CSI M`, 1000/1002/1003, SGR 1006, SGR-Pixels 1016), focus events
//! and alternate scroll (INPUT-18, -19, -22, -23, -24).
//!
//! Not built: 1005, 1015 and 9 (INPUT-20 -> never); those bits are ignored here.

use crate::buf::W;
use crate::key::{Key, KeyEvent, KeyKind, Mods, Named};
use crate::modes::{InputModes, MouseMode, bit};

/// Mouse button. Codes: left/middle/right 0/1/2, wheel up/down/left/right 64..67,
/// buttons 8..11 are 128..131 (the press code is identical in X10 and SGR; releases differ,
/// see [`encode_mouse_outcome`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// Left.
    Left,
    /// Middle.
    Middle,
    /// Right.
    Right,
    /// Wheel up.
    WheelUp,
    /// Wheel down.
    WheelDown,
    /// Wheel left.
    WheelLeft,
    /// Wheel right.
    WheelRight,
    /// Extra button 8..=11.
    Extra(u8),
}

impl MouseButton {
    /// Protocol button code.
    pub const fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
            MouseButton::Extra(n) => {
                let n = if n < 8 {
                    8
                } else if n > 11 {
                    11
                } else {
                    n
                };
                128 + (n as u32 - 8)
            }
        }
    }
    /// Is a wheel button.
    pub const fn is_wheel(self) -> bool {
        matches!(
            self,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        )
    }
}

/// Event type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    /// Button down (wheel notches are presses).
    Press,
    /// Button up.
    Release,
    /// Pointer motion; `button` is the held button, if any.
    Motion,
}

/// One pointer event. Cell coordinates are 0-based; pixel coordinates (used under 1016)
/// are 0-based and relative to the text area, from the caller's exact cell metrics (INPUT-19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    /// Button (held button for motion).
    pub button: Option<MouseButton>,
    /// Press, release or motion.
    pub kind: MouseKind,
    /// Modifiers (only shift, alt, ctrl are reported).
    pub mods: Mods,
    /// Cell column, 0-based.
    pub col: u32,
    /// Cell row, 0-based.
    pub row: u32,
    /// Pixel x in the text area, 0-based.
    pub px: u32,
    /// Pixel y in the text area, 0-based.
    pub py: u32,
}

/// What [`encode_mouse_outcome`] did with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseOutcome {
    /// This many bytes were written to the buffer.
    Sent(usize),
    /// The application asked for this event but the X10 encoding cannot carry it (column
    /// or row above 223). Nothing is sent and the host must not treat the event as a
    /// local-selection start (INPUT-22): the application owns the mouse.
    Suppressed,
    /// The event is not reportable in the current mode (reporting off, or an event kind
    /// the mode does not report); the host may handle it locally.
    NotReported,
}

/// Encode a mouse event. Returns the byte count; **0 means no event was written**. Use
/// [`encode_mouse_outcome`] to tell an X10 overflow (application still owns the mouse)
/// from "not reported" (host may start a local selection, INPUT-22). The host decides
/// Shift override first with [`selection_override`].
pub fn encode_mouse(ev: &MouseEvent, modes: InputModes, out: &mut [u8; 32]) -> usize {
    match encode_mouse_outcome(ev, modes, out) {
        MouseOutcome::Sent(n) => n,
        _ => 0,
    }
}

/// [`encode_mouse`] with a distinct result for events suppressed by the X10 range limit.
///
/// Releases: SGR keeps the pressed button's code with a final `m`; X10 has no per-button
/// release, so it sends low bits 3 and keeps only the modifier bits (wheel and extra
/// button bits are not carried). The two encodings therefore differ on release.
pub fn encode_mouse_outcome(
    ev: &MouseEvent,
    modes: InputModes,
    out: &mut [u8; 32],
) -> MouseOutcome {
    let mode = modes.mouse_mode();
    if mode == MouseMode::None {
        return MouseOutcome::NotReported;
    }
    let wheel = ev.button.is_some_and(MouseButton::is_wheel);
    match ev.kind {
        MouseKind::Release if wheel => return MouseOutcome::NotReported,
        MouseKind::Press | MouseKind::Release if ev.button.is_none() => {
            return MouseOutcome::NotReported;
        }
        MouseKind::Motion if mode == MouseMode::Click => return MouseOutcome::NotReported,
        MouseKind::Motion if mode == MouseMode::Drag && ev.button.is_none() => {
            return MouseOutcome::NotReported;
        }
        MouseKind::Motion if wheel => return MouseOutcome::NotReported,
        _ => {}
    }
    let mut code = ev.button.map_or(3, MouseButton::code);
    if ev.mods.contains(Mods::SHIFT) {
        code |= 4;
    }
    if ev.mods.contains(Mods::ALT) {
        code |= 8;
    }
    if ev.mods.contains(Mods::CTRL) {
        code |= 16;
    }
    if ev.kind == MouseKind::Motion {
        code |= 32;
    }
    let pixels = modes.has(bit::MOUSE_PIXELS_1016);
    let (x, y) = if pixels {
        (ev.px, ev.py)
    } else {
        (ev.col, ev.row)
    };
    let (x, y) = (x.saturating_add(1), y.saturating_add(1));
    let mut w = W::new(out);
    if pixels || modes.has(bit::MOUSE_SGR_1006) {
        w.bytes(b"\x1b[<");
        w.num(code);
        w.byte(b';');
        w.num(x);
        w.byte(b';');
        w.num(y);
        w.byte(if ev.kind == MouseKind::Release {
            b'm'
        } else {
            b'M'
        });
        return MouseOutcome::Sent(w.len());
    }
    // X10 `CSI M Cb Cx Cy`: release has button code 3; coordinates above 223 are not sent.
    if x > 223 || y > 223 {
        return MouseOutcome::Suppressed;
    }
    if ev.kind == MouseKind::Release {
        code = (code & !0b11) | 3;
        code &= !(0x80 | 0x40);
    }
    let cb = 32 + code;
    if cb > 255 {
        return MouseOutcome::Suppressed;
    }
    w.bytes(b"\x1b[M");
    w.byte(cb as u8);
    w.byte((32 + x) as u8);
    w.byte((32 + y) as u8);
    MouseOutcome::Sent(w.len())
}

/// Same-position motion suppression state (one per pane).
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseTracker {
    last: Option<(u32, u32)>,
}

impl MouseTracker {
    /// Should a motion event at this position be encoded? Compares cells, or pixels under 1016.
    /// Call [`MouseTracker::reset`] on press/release/leave.
    pub fn motion_changed(&mut self, ev: &MouseEvent, modes: InputModes) -> bool {
        let pos = if modes.has(bit::MOUSE_PIXELS_1016) {
            (ev.px, ev.py)
        } else {
            (ev.col, ev.row)
        };
        if self.last == Some(pos) {
            return false;
        }
        self.last = Some(pos);
        true
    }
    /// Forget the last position.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Does the held modifier set override mouse capture for local selection? Shift is the
/// default override set (`mouse.selection-override`); an empty set never overrides.
pub fn selection_override(held: Mods, override_set: Mods) -> bool {
    let h = held.without_locks();
    !override_set.is_empty() && h.contains(override_set)
}

/// Focus event (mode 1004): `CSI I` / `CSI O`; 0 when 1004 is not set.
pub fn encode_focus(focused: bool, modes: InputModes, out: &mut [u8; 3]) -> usize {
    if !modes.has(bit::FOCUS_1004) {
        return 0;
    }
    out.copy_from_slice(&[0x1b, b'[', if focused { b'I' } else { b'O' }]);
    3
}

/// What the wheel does right now (T-05 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAction {
    /// Report as a mouse button through [`encode_mouse`].
    Report,
    /// Send `mouse.scroll-lines` Up/Down key presses via [`encode_wheel_key`] per notch.
    AltScrollKeys,
    /// Scroll the viewport.
    Viewport,
}

/// Wheel routing: reported while 1000/1002/1003 is set; otherwise keys on the alternate
/// screen with 1007 set; otherwise the viewport.
pub fn wheel_action(modes: InputModes) -> WheelAction {
    if modes.mouse_mode() != MouseMode::None {
        WheelAction::Report
    } else if modes.has(bit::ALT_SCREEN) && modes.has(bit::ALT_SCROLL_1007) {
        WheelAction::AltScrollKeys
    } else {
        WheelAction::Viewport
    }
}

/// One alternate-scroll notch: an Up/Down key press through the key encoder (DECCKM form,
/// kitty flags apply). The caller repeats it `mouse.scroll-lines` times.
pub fn encode_wheel_key(up: bool, mods: Mods, modes: InputModes, out: &mut [u8; 64]) -> usize {
    let n = if up { Named::Up } else { Named::Down };
    let ev = KeyEvent::new(Key::Named(n)).mods(mods).kind(KeyKind::Press);
    crate::encode_key(&ev, modes, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(button: Option<MouseButton>, kind: MouseKind, col: u32, row: u32) -> MouseEvent {
        MouseEvent {
            button,
            kind,
            mods: Mods::NONE,
            col,
            row,
            px: col.wrapping_mul(10).wrapping_add(3),
            py: row.wrapping_mul(20).wrapping_add(5),
        }
    }
    fn enc(e: &MouseEvent, m: InputModes) -> Vec<u8> {
        let mut out = [0u8; 32];
        let n = encode_mouse(e, m, &mut out);
        out[..n].to_vec()
    }
    fn mode(bits: u64) -> InputModes {
        InputModes::default().with(bits, true)
    }
    const P: MouseKind = MouseKind::Press;
    const R: MouseKind = MouseKind::Release;
    const M: MouseKind = MouseKind::Motion;
    const L: Option<MouseButton> = Some(MouseButton::Left);

    #[test]
    fn off_writes_nothing() {
        assert!(enc(&ev(L, P, 0, 0), InputModes::default()).is_empty());
        assert!(enc(&ev(L, P, 0, 0), mode(bit::MOUSE_SGR_1006)).is_empty());
        // Modes 9, 1005, 1015 are not built.
        assert!(
            enc(
                &ev(L, P, 0, 0),
                mode(bit::MOUSE_9 | bit::MOUSE_1005 | bit::MOUSE_1015)
            )
            .is_empty()
        );
    }

    #[test]
    fn x10_default_encoding() {
        let m = mode(bit::MOUSE_1000);
        assert_eq!(enc(&ev(L, P, 0, 0), m), [0x1b, b'[', b'M', 32, 33, 33]);
        assert_eq!(
            enc(&ev(Some(MouseButton::Middle), P, 4, 9), m),
            [0x1b, b'[', b'M', 33, 37, 42]
        );
        assert_eq!(
            enc(&ev(Some(MouseButton::Right), P, 1, 1), m),
            [0x1b, b'[', b'M', 34, 34, 34]
        );
        // Release is button 3.
        assert_eq!(enc(&ev(L, R, 0, 0), m), [0x1b, b'[', b'M', 35, 33, 33]);
    }

    #[test]
    fn x10_modifiers() {
        let m = mode(bit::MOUSE_1000);
        let mut e = ev(L, P, 0, 0);
        e.mods = Mods::SHIFT;
        assert_eq!(enc(&e, m)[3], 32 + 4);
        e.mods = Mods::ALT;
        assert_eq!(enc(&e, m)[3], 32 + 8);
        e.mods = Mods::CTRL;
        assert_eq!(enc(&e, m)[3], 32 + 16);
        e.mods = Mods::SHIFT | Mods::ALT | Mods::CTRL | Mods::CAPS;
        assert_eq!(enc(&e, m)[3], 32 + 28);
    }

    #[test]
    fn x10_223_limit() {
        let m = mode(bit::MOUSE_1000);
        // 0-based 222 is the last reportable cell (1-based 223 -> byte 255).
        assert_eq!(
            enc(&ev(L, P, 222, 222), m),
            [0x1b, b'[', b'M', 32, 255, 255]
        );
        assert!(enc(&ev(L, P, 223, 0), m).is_empty());
        assert!(enc(&ev(L, P, 0, 223), m).is_empty());
        assert!(enc(&ev(L, R, 500, 500), m).is_empty());
        // Overflow is "suppressed" (app owns the mouse), unlike "not reported".
        let mut out = [0u8; 32];
        assert_eq!(
            encode_mouse_outcome(&ev(L, P, 223, 0), m, &mut out),
            MouseOutcome::Suppressed
        );
        assert_eq!(
            encode_mouse_outcome(&ev(L, P, 222, 0), m, &mut out),
            MouseOutcome::Sent(6)
        );
        assert_eq!(
            encode_mouse_outcome(&ev(L, P, 0, 0), InputModes::default(), &mut out),
            MouseOutcome::NotReported
        );
        // Click-only mode does not report motion: not suppressed.
        assert_eq!(
            encode_mouse_outcome(&ev(L, M, 500, 0), m, &mut out),
            MouseOutcome::NotReported
        );
        // X10 release: low bits 3, modifiers kept, extra-button bits dropped; SGR keeps them.
        let mut e = ev(Some(MouseButton::Extra(9)), R, 0, 0);
        e.mods = Mods::CTRL;
        assert_eq!(enc(&e, m), [0x1b, b'[', b'M', 32 + 3 + 16, 33, 33]);
        let s = mode(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        assert_eq!(enc(&e, s), b"\x1b[<145;1;1m");
        // SGR has no cap.
        let s = mode(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        assert_eq!(enc(&ev(L, P, 499, 999), s), b"\x1b[<0;500;1000M");
    }

    #[test]
    fn sgr_encoding() {
        let m = mode(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        assert_eq!(enc(&ev(L, P, 0, 0), m), b"\x1b[<0;1;1M");
        assert_eq!(enc(&ev(L, R, 0, 0), m), b"\x1b[<0;1;1m");
        assert_eq!(
            enc(&ev(Some(MouseButton::Right), R, 9, 4), m),
            b"\x1b[<2;10;5m"
        );
        let mut e = ev(L, P, 2, 3);
        e.mods = Mods::CTRL | Mods::SHIFT;
        assert_eq!(enc(&e, m), b"\x1b[<20;3;4M");
    }

    #[test]
    fn button_codes() {
        let s = mode(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        for (b, c) in [
            (MouseButton::WheelUp, 64),
            (MouseButton::WheelDown, 65),
            (MouseButton::WheelLeft, 66),
            (MouseButton::WheelRight, 67),
            (MouseButton::Extra(8), 128),
            (MouseButton::Extra(9), 129),
            (MouseButton::Extra(10), 130),
            (MouseButton::Extra(11), 131),
        ] {
            assert_eq!(
                enc(&ev(Some(b), P, 0, 0), s),
                format!("\x1b[<{c};1;1M").as_bytes()
            );
        }
        // Identical in X10 (code + 32).
        let x = mode(bit::MOUSE_1000);
        assert_eq!(enc(&ev(Some(MouseButton::WheelUp), P, 0, 0), x)[3], 32 + 64);
        assert_eq!(
            enc(&ev(Some(MouseButton::Extra(11)), P, 0, 0), x)[3],
            32 + 131
        );
        // Wheel has no release.
        assert!(enc(&ev(Some(MouseButton::WheelUp), R, 0, 0), s).is_empty());
        assert!(enc(&ev(Some(MouseButton::WheelUp), R, 0, 0), x).is_empty());
    }

    #[test]
    fn motion_by_mode() {
        let click = mode(bit::MOUSE_1000 | bit::MOUSE_SGR_1006);
        let drag = mode(bit::MOUSE_1002 | bit::MOUSE_SGR_1006);
        let any = mode(bit::MOUSE_1003 | bit::MOUSE_SGR_1006);
        assert!(enc(&ev(L, M, 1, 1), click).is_empty());
        assert_eq!(enc(&ev(L, M, 1, 1), drag), b"\x1b[<32;2;2M");
        assert!(enc(&ev(None, M, 1, 1), drag).is_empty());
        assert_eq!(enc(&ev(None, M, 1, 1), any), b"\x1b[<35;2;2M");
        assert_eq!(
            enc(&ev(Some(MouseButton::Right), M, 1, 1), any),
            b"\x1b[<34;2;2M"
        );
        // Highest mode wins.
        assert_eq!(
            enc(
                &ev(None, M, 0, 0),
                mode(bit::MOUSE_1000 | bit::MOUSE_1003 | bit::MOUSE_SGR_1006)
            ),
            b"\x1b[<35;1;1M"
        );
    }

    #[test]
    fn sgr_pixels() {
        let m = mode(bit::MOUSE_1003 | bit::MOUSE_PIXELS_1016);
        assert_eq!(enc(&ev(L, P, 2, 1), m), b"\x1b[<0;24;26M");
        assert_eq!(enc(&ev(L, R, 2, 1), m), b"\x1b[<0;24;26m");
        // 1016 wins over 1006 and has no cap.
        let both = mode(bit::MOUSE_1000 | bit::MOUSE_SGR_1006 | bit::MOUSE_PIXELS_1016);
        let mut e = ev(L, P, 0, 0);
        e.px = 5000;
        e.py = 3000;
        assert_eq!(enc(&e, both), b"\x1b[<0;5001;3001M");
    }

    #[test]
    fn tracker_dedups() {
        let mut t = MouseTracker::default();
        let m = mode(bit::MOUSE_1003 | bit::MOUSE_SGR_1006);
        assert!(t.motion_changed(&ev(None, M, 1, 1), m));
        let mut same_cell = ev(None, M, 1, 1);
        same_cell.px += 2;
        assert!(!t.motion_changed(&same_cell, m));
        assert!(t.motion_changed(&ev(None, M, 2, 1), m));
        t.reset();
        assert!(t.motion_changed(&ev(None, M, 2, 1), m));
        // Pixel mode compares pixels.
        let p = mode(bit::MOUSE_1003 | bit::MOUSE_PIXELS_1016);
        t.reset();
        assert!(t.motion_changed(&ev(None, M, 1, 1), p));
        assert!(t.motion_changed(&same_cell, p));
    }

    #[test]
    fn override_and_focus() {
        assert!(selection_override(Mods::SHIFT, Mods::SHIFT));
        assert!(selection_override(
            Mods::SHIFT | Mods::CTRL | Mods::CAPS,
            Mods::SHIFT
        ));
        assert!(!selection_override(Mods::CTRL, Mods::SHIFT));
        assert!(!selection_override(Mods::SHIFT, Mods::SHIFT | Mods::ALT));
        assert!(!selection_override(Mods::SHIFT, Mods::NONE));

        let mut out = [0u8; 3];
        assert_eq!(encode_focus(true, InputModes::default(), &mut out), 0);
        let m = mode(bit::FOCUS_1004);
        assert_eq!(encode_focus(true, m, &mut out), 3);
        assert_eq!(&out, b"\x1b[I");
        assert_eq!(encode_focus(false, m, &mut out), 3);
        assert_eq!(&out, b"\x1b[O");
    }

    #[test]
    fn wheel_routing() {
        let alt = mode(bit::ALT_SCREEN);
        assert_eq!(wheel_action(InputModes::default()), WheelAction::Viewport);
        assert_eq!(wheel_action(alt), WheelAction::AltScrollKeys);
        assert_eq!(
            wheel_action(alt.with(bit::ALT_SCROLL_1007, false)),
            WheelAction::Viewport
        );
        assert_eq!(
            wheel_action(alt.with(bit::MOUSE_1000, true)),
            WheelAction::Report
        );
        assert_eq!(wheel_action(mode(bit::MOUSE_1003)), WheelAction::Report);
        // Mode 9 does not count as reporting.
        assert_eq!(
            wheel_action(alt.with(bit::MOUSE_9, true)),
            WheelAction::AltScrollKeys
        );
    }

    #[test]
    fn wheel_keys_use_key_encoder() {
        let mut out = [0u8; 64];
        let m = mode(bit::ALT_SCREEN);
        let n = encode_wheel_key(true, Mods::NONE, m, &mut out);
        assert_eq!(&out[..n], b"\x1b[A");
        let n = encode_wheel_key(false, Mods::NONE, m.with(bit::DECCKM, true), &mut out);
        assert_eq!(&out[..n], b"\x1bOB");
        // Kitty flags apply.
        let n = encode_wheel_key(true, Mods::NONE, m.with_kitty(1), &mut out);
        assert_eq!(&out[..n], b"\x1b[A");
        let n = encode_wheel_key(true, Mods::CTRL, m.with_kitty(1), &mut out);
        assert_eq!(&out[..n], b"\x1b[1;5A");
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let mut e = ev(L, P, u32::MAX, u32::MAX);
        e.px = u32::MAX;
        e.py = u32::MAX;
        for bits in [
            bit::MOUSE_1000,
            bit::MOUSE_1003 | bit::MOUSE_SGR_1006,
            bit::MOUSE_1002 | bit::MOUSE_PIXELS_1016,
        ] {
            let n = enc(&e, mode(bits)).len();
            assert!(n <= 32);
        }
    }
}
