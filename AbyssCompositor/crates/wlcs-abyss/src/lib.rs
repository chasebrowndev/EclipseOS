// SPDX-License-Identifier: AGPL-3.0-only
//! wlcs conformance integration (COMP-15 §1, Vol 1 §15).
//!
//! wlcs `dlopen`s this library and reads one symbol,
//! `wlcs_server_integration`, from it. For each test it asks for a compositor,
//! gets client socket pairs and synthetic input devices from it, and tears it
//! down again.
//!
//! Every compositor gets its own OS thread running the ordinary headless
//! backend. The wlcs thread never touches compositor state: it only pushes
//! [`WlcsEvent`]s down a channel that the compositor's own calloop loop drains,
//! so the single-threaded-core invariant survives contact with a
//! multi-threaded test runner.

use std::io::{Error, ErrorKind};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;

use abyss::backend::headless::{wlcs_channel, WlcsEvent, WlcsSender};
use wayland_sys::client::*;
use wayland_sys::ffi_dispatch;
use wlcs::{
    extension_list,
    ffi_display_server_api::{WlcsExtensionDescriptor, WlcsIntegrationDescriptor, WlcsServerIntegration},
    ffi_wrappers::wlcs_server,
    wlcs_server_integration, Wlcs,
};

wlcs_server_integration!(AbyssHandle);

/// What abyss advertises and wlcs is therefore allowed to test. Versions match
/// the globals in `protocols/standard/`; claiming one abyss does not implement
/// turns a missing feature into a confusing protocol error instead of a
/// skipped test.
static SUPPORTED_EXTENSIONS: &[WlcsExtensionDescriptor] = extension_list!(
    ("wl_compositor", 6),
    ("wl_subcompositor", 1),
    ("wl_data_device_manager", 3),
    ("wl_seat", 9),
    ("wl_output", 4),
    ("xdg_wm_base", 6),
    ("zwp_text_input_manager_v3", 1),
    ("zwp_input_method_manager_v2", 1),
);

static DESCRIPTOR: WlcsIntegrationDescriptor = WlcsIntegrationDescriptor {
    version: 1,
    num_extensions: SUPPORTED_EXTENSIONS.len(),
    supported_extensions: SUPPORTED_EXTENSIONS.as_ptr(),
};

/// One compositor, alive for one test.
struct AbyssHandle {
    server: Option<(WlcsSender, JoinHandle<()>)>,
}

impl Wlcs for AbyssHandle {
    type Pointer = PointerHandle;
    type Touch = TouchHandle;

    fn new() -> Self {
        Self { server: None }
    }

    fn start(&mut self) {
        let (tx, rx) = wlcs_channel();
        let join = std::thread::spawn(move || {
            if let Err(e) = abyss::backend::headless::run_wlcs(rx) {
                eprintln!("wlcs-abyss: compositor exited with an error: {e:?}");
            }
        });
        self.server = Some((tx, join));
    }

    fn stop(&mut self) {
        if let Some((sender, join)) = self.server.take() {
            let _ = sender.send(WlcsEvent::Exit);
            let _ = join.join();
        }
    }

    fn create_client_socket(&self) -> std::io::Result<OwnedFd> {
        let Some((sender, _)) = self.server.as_ref() else {
            return Err(Error::from(ErrorKind::NotFound));
        };
        let (client_side, server_side) = UnixStream::pair()?;
        // wlcs identifies the client by the fd number it holds, so that number
        // has to be captured before the fd is handed over.
        let client_id = client_side.as_raw_fd();
        sender
            .send(WlcsEvent::NewClient {
                stream: server_side,
                client_id,
            })
            .map_err(|e| Error::new(ErrorKind::ConnectionReset, e))?;
        Ok(client_side.into())
    }

    fn position_window_absolute(&self, display: *mut wl_display, surface: *mut wl_proxy, x: i32, y: i32) {
        // SAFETY: wlcs owns both pointers and they are live for this call.
        let client_id = unsafe { ffi_dispatch!(wayland_client_handle(), wl_display_get_fd, display) };
        let surface_id = unsafe { ffi_dispatch!(wayland_client_handle(), wl_proxy_get_id, surface) };
        if let Some((sender, _)) = self.server.as_ref() {
            let _ = sender.send(WlcsEvent::PositionWindow {
                client_id,
                surface_id,
                location: (x, y),
            });
        }
    }

    fn create_pointer(&mut self) -> Option<Self::Pointer> {
        let (sender, _) = self.server.as_ref()?;
        Some(PointerHandle {
            sender: sender.clone(),
        })
    }

    fn create_touch(&mut self) -> Option<Self::Touch> {
        let (sender, _) = self.server.as_ref()?;
        Some(TouchHandle {
            sender: sender.clone(),
        })
    }

    fn get_descriptor(&self) -> &WlcsIntegrationDescriptor {
        &DESCRIPTOR
    }
}

/// wlcs's synthetic pointer. There is only one seat, so the device id wlcs
/// assigns is not carried across — every pointer drives the same seat.
struct PointerHandle {
    sender: WlcsSender,
}

/// `wl_fixed_t` is 24.8 fixed point.
fn fixed(v: i32) -> f64 {
    v as f64 / 256.0
}

impl wlcs::Pointer for PointerHandle {
    fn move_absolute(&mut self, x: i32, y: i32) {
        let _ = self.sender.send(WlcsEvent::PointerMoveAbsolute {
            location: (fixed(x), fixed(y)),
        });
    }

    fn move_relative(&mut self, dx: i32, dy: i32) {
        let _ = self.sender.send(WlcsEvent::PointerMoveRelative {
            delta: (fixed(dx), fixed(dy)),
        });
    }

    fn button_down(&mut self, button: i32) {
        let _ = self
            .sender
            .send(WlcsEvent::PointerButtonDown { button_id: button });
    }

    fn button_up(&mut self, button: i32) {
        let _ = self.sender.send(WlcsEvent::PointerButtonUp { button_id: button });
    }
}

/// wlcs's synthetic touch device. The suite only ever drives a single touch
/// point, so every event goes to slot 0.
struct TouchHandle {
    sender: WlcsSender,
}

const WLCS_TOUCH_SLOT: u32 = 0;

// wlcs declares the touch hooks as taking `wl_fixed_t` (`include/wlcs/touch.h`)
// but hands them plain pixel ints (`src/in_process_server.cpp:271`, `down_at`).
// So touch coordinates are *not* 24.8 fixed point the way the pointer ones are:
// dividing by 256 here put every touch at (0.35, 0.05) and no surface was hit.

impl wlcs::Touch for TouchHandle {
    fn touch_down(&mut self, x: i32, y: i32) {
        let _ = self.sender.send(WlcsEvent::TouchDown {
            slot: WLCS_TOUCH_SLOT,
            location: (x as f64, y as f64),
        });
    }

    fn touch_move(&mut self, x: i32, y: i32) {
        let _ = self.sender.send(WlcsEvent::TouchMove {
            slot: WLCS_TOUCH_SLOT,
            location: (x as f64, y as f64),
        });
    }

    fn touch_up(&mut self) {
        let _ = self.sender.send(WlcsEvent::TouchUp {
            slot: WLCS_TOUCH_SLOT,
        });
    }
}
