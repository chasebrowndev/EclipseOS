// SPDX-License-Identifier: AGPL-3.0-only
//! Test-only driver for the headless backend (feature `smoke-input`, never in
//! a release build): a Unix socket at `$ECLIPSE_HEADLESS_INPUT` that feeds
//! *physical-origin* input into the loop and can dump a frame, so an
//! end-to-end script can play the human. A headless session has no input
//! device and `injected` input cannot reach a protected surface (COMP-19 §3),
//! so without this a script cannot press Enter on a commit slot.
//!
//! One command per line: `key <evdev code> <0|1>`, `move <x> <y>`,
//! `button <evdev code> <0|1>`, `dump <path.pam>`. Nothing is logged by value.

use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use smithay::backend::input::{ButtonState, KeyState, Keycode};
use smithay::backend::renderer::{gles::GlesRenderer, Bind, ExportMem};
use smithay::reexports::calloop::{
    channel::{channel, Event, Sender},
    LoopHandle,
};
use smithay::utils::{Rectangle, Size};

use crate::input::Origin;
use crate::state::AbyssState;

enum Cmd {
    Key(u32, bool),
    Move(f64, f64),
    Button(u32, bool),
    Dump(PathBuf),
}

thread_local! {
    static DUMP: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn parse(line: &str) -> Option<Cmd> {
    let mut w = line.split_whitespace();
    Some(match w.next()? {
        "key" => Cmd::Key(w.next()?.parse().ok()?, w.next()? == "1"),
        "move" => Cmd::Move(w.next()?.parse().ok()?, w.next()?.parse().ok()?),
        "button" => Cmd::Button(w.next()?.parse().ok()?, w.next()? == "1"),
        "dump" => Cmd::Dump(PathBuf::from(w.next()?)),
        _ => return None,
    })
}

pub fn start(handle: &LoopHandle<'static, AbyssState>) {
    let Some(path) = std::env::var_os("ECLIPSE_HEADLESS_INPUT") else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(%e, "smoke-input: bind");
            return;
        }
    };
    let (tx, rx): (Sender<Cmd>, _) = channel();
    let _ = handle.insert_source(rx, |ev, _, state| {
        let Event::Msg(cmd) = ev else { return };
        let t = state.start_time.elapsed().as_millis() as u32;
        match cmd {
            Cmd::Key(code, down) => state.with_origin(Origin::Physical, |s| {
                s.keyboard_key(
                    Keycode::new(code + 8),
                    if down {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    },
                    t,
                )
            }),
            Cmd::Move(x, y) => state.with_origin(Origin::Physical, |s| s.pointer_moved((x, y).into(), t)),
            Cmd::Button(code, down) => state.with_origin(Origin::Physical, |s| {
                s.pointer_button(
                    code,
                    if down {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                    t,
                )
            }),
            Cmd::Dump(p) => DUMP.with(|d| *d.borrow_mut() = Some(p)),
        }
    });
    tracing::info!("smoke-input: listening (test-only physical input driver)");
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    if let Some(c) = parse(&line) {
                        let _ = tx.send(c);
                    }
                }
            });
        }
    });
}

/// After a frame is composited into `target`: write it out if asked.
pub fn after_render(
    renderer: &mut GlesRenderer,
    target: &mut smithay::backend::renderer::gles::GlesTexture,
    size: Size<i32, smithay::utils::Buffer>,
) {
    let Some(path) = DUMP.with(|d| d.borrow_mut().take()) else {
        return;
    };
    let Ok(fb) = Bind::bind(renderer, target) else {
        return;
    };
    let region = Rectangle::from_size(size);
    let Ok(mapping) = ExportMem::copy_framebuffer(renderer, &fb, region, crate::render::capture::FORMAT)
    else {
        return;
    };
    let Ok(px) = ExportMem::map_texture(renderer, &mapping) else {
        return;
    };
    // Xbgr8888 is R,G,B,X in memory; PAM wants RGB_ALPHA.
    let mut out = format!(
        "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
        size.w, size.h
    )
    .into_bytes();
    for p in px.chunks_exact(4) {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    let _ = std::fs::write(path, out);
}
