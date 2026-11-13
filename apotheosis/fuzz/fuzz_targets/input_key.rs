// SPDX-License-Identifier: AGPL-3.0-only
//! PERFORMANCE-32: key encoder over (key, mods, kind, mode bits, kitty flags, modifyOtherKeys).
//! Output is bounded (never writes past 64), deterministic, and a Release without kitty
//! report-events flag is no more than legacy would send.
#![no_main]
use apoth_input::{InputModes, Key, KeyEvent, KeyKind, Kp, Mods, Named, encode_key};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;

fn named(u: &mut Unstructured<'_>) -> Result<Named> {
    Ok(match u.int_in_range(0..=24u8)? {
        0 => Named::Escape,
        1 => Named::Enter,
        2 => Named::Tab,
        3 => Named::Backspace,
        4 => Named::Insert,
        5 => Named::Delete,
        6 => Named::Left,
        7 => Named::Right,
        8 => Named::Up,
        9 => Named::Down,
        10 => Named::PageUp,
        11 => Named::PageDown,
        12 => Named::Home,
        13 => Named::End,
        14 => Named::F(u.arbitrary()?),
        15 => Named::Menu,
        16 => Named::CapsLock,
        17 => Named::ScrollLock,
        18 => Named::NumLock,
        19 => Named::PrintScreen,
        20 => Named::Pause,
        21 => Named::Kp(Kp::Digit(u.arbitrary()?)),
        22 => Named::Kp(Kp::Enter),
        23 => Named::Kp(Kp::Begin),
        _ => Named::Pua(u.arbitrary()?),
    })
}

fn go(u: &mut Unstructured<'_>) -> Result<()> {
    let key = if u.arbitrary()? {
        Key::Char(u.arbitrary()?)
    } else {
        Key::Named(named(u)?)
    };
    let kind = match u.int_in_range(0..=2u8)? {
        0 => KeyKind::Press,
        1 => KeyKind::Repeat,
        _ => KeyKind::Release,
    };
    let text_len = u.int_in_range(0..=8usize)?;
    let text: String = (0..text_len).map(|_| u.arbitrary::<char>()).collect::<Result<_>>()?;
    let mut ev = KeyEvent::new(key).mods(Mods(u.arbitrary()?)).kind(kind).text(&text);
    if u.arbitrary()? {
        ev = ev.shifted(u.arbitrary()?);
    }
    if u.arbitrary()? {
        ev = ev.base(u.arbitrary()?);
    }
    if u.arbitrary()? {
        ev = ev.ctrl_fallback(u.arbitrary()?);
    }
    let modes = InputModes::from_raw(u.arbitrary()?, u.arbitrary()?)
        .with_mok_level(u.int_in_range(0..=3u8)?);
    let mut a = [0u8; 64];
    let mut b = [0xFFu8; 64];
    let na = encode_key(&ev, modes, &mut a);
    let nb = encode_key(&ev, modes, &mut b);
    assert!(na <= 64);
    assert_eq!(na, nb);
    assert_eq!(a[..na], b[..nb], "non-deterministic");
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let _ = go(&mut Unstructured::new(data));
});
