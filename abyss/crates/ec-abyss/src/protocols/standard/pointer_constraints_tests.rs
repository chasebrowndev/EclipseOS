// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_relative_pointer_v1` and `zwp_pointer_constraints_v1` against the
//! in-process harness (COMP-06 §1), driven the way a game is: lock the pointer,
//! then expect physical device motion to arrive as relative deltas. Xwayland
//! relies on exactly this for X11 mouse-look (warp emulation and XI2 raw
//! motion), so a regression here breaks every Proton game.

use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;

use smithay::backend::input::{
    Device, DeviceCapability, Event, InputBackend, InputEvent, PointerMotionEvent, UnusedEvent,
};
use smithay::utils::{Logical, Point};
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_pointer::{self, WlPointer},
        wl_registry,
        wl_seat::{self, WlSeat},
        wl_shm,
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use crate::shell::focus::state_tests::{harness, Harness};
use crate::state::AbyssState;

const WIN: (i32, i32) = (300, 200);

#[derive(Default)]
struct Seen {
    globals: Vec<(u32, String, u32)>,
    pointer: Option<WlPointer>,
    ptr_enters: u32,
    locked: u32,
    /// Accelerated deltas, as delivered on `zwp_relative_pointer_v1`.
    relative: Vec<(f64, f64)>,
    /// Unaccelerated deltas from the same events.
    relative_unaccel: Vec<(f64, f64)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            s.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        _: &mut Self,
        xdg: &XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(s: &mut Self, seat: &WlSeat, e: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = e
        {
            if caps.contains(wl_seat::Capability::Pointer) && s.pointer.is_none() {
                s.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Enter { .. } = e {
            s.ptr_enters += 1;
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &ZwpRelativePointerV1,
        e: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
            ..
        } = e
        {
            s.relative.push((dx, dy));
            s.relative_unaccel.push((dx_unaccel, dy_unaccel));
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &ZwpLockedPointerV1,
        e: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_locked_pointer_v1::Event::Locked = e {
            s.locked += 1;
        }
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore wl_shm::WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore XdgToplevel);
delegate_noop!(Seen: ignore ZwpRelativePointerManagerV1);
delegate_noop!(Seen: ignore ZwpPointerConstraintsV1);

// ---- a libinput-shaped backend with nothing but relative motion

struct Fake;

#[derive(PartialEq, Eq, Hash)]
struct Mouse;

impl Device for Mouse {
    fn id(&self) -> String {
        "mouse".into()
    }
    fn name(&self) -> String {
        "mouse".into()
    }
    fn has_capability(&self, c: DeviceCapability) -> bool {
        c == DeviceCapability::Pointer
    }
    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }
    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// One motion event: accelerated delta, then unaccelerated.
struct Motion((f64, f64), (f64, f64));

impl Event<Fake> for Motion {
    fn time(&self) -> u64 {
        0
    }
    fn device(&self) -> Mouse {
        Mouse
    }
}

impl PointerMotionEvent<Fake> for Motion {
    fn delta_x(&self) -> f64 {
        self.0 .0
    }
    fn delta_y(&self) -> f64 {
        self.0 .1
    }
    fn delta_x_unaccel(&self) -> f64 {
        self.1 .0
    }
    fn delta_y_unaccel(&self) -> f64 {
        self.1 .1
    }
}

impl InputBackend for Fake {
    type Device = Mouse;
    type KeyboardKeyEvent = UnusedEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerMotionEvent = Motion;
    type PointerMotionAbsoluteEvent = UnusedEvent;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = ();
}

fn mouse(state: &mut AbyssState, accel: (f64, f64), unaccel: (f64, f64)) {
    state.process_input_event::<Fake>(InputEvent::PointerMotion {
        event: Motion(accel, unaccel),
    });
}

// ---- the client

struct Rig {
    h: Harness,
    conn: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: WlSurface,
    _xdg: XdgSurface,
    _toplevel: XdgToplevel,
    _relative: ZwpRelativePointerV1,
    constraints: ZwpPointerConstraintsV1,
}

impl Rig {
    fn pump(&mut self) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            self.h.dispatch();
            if let Some(g) = self.conn.prepare_read() {
                let _ = g.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.seen);
        }
    }

    /// Global top-left of the mapped window's surface.
    fn origin(&self) -> Point<f64, Logical> {
        let s = self.h.state.xdg_shell_state.toplevel_surfaces()[0]
            .wl_surface()
            .clone();
        crate::protocols::protected::surface_origin(&self.h.state, &s)
            .expect("mapped")
            .to_f64()
    }

    fn lock(&mut self) -> ZwpLockedPointerV1 {
        let qh = self.queue.handle();
        let pointer = self.seen.pointer.clone().expect("wl_pointer");
        let lock =
            self.constraints
                .lock_pointer(&self.surface, &pointer, None, Lifetime::Persistent, &qh, ());
        self.pump();
        assert_eq!(self.seen.locked, 1, "lock engaged on the pointer-focused surface");
        lock
    }
}

/// A client with one mapped, focused window under the pointer and a
/// relative pointer bound.
fn rig() -> Rig {
    let mut h = harness();
    let (server, client) = UnixStream::pair().expect("socket pair");
    h.state
        .display_handle
        .insert_client(server, crate::state::client_state())
        .expect("insert client");
    let conn = Connection::from_socket(client).expect("connect");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut seen = Seen::default();
    let pump = |seen: &mut Seen, queue: &mut EventQueue<Seen>, h: &mut Harness| {
        for _ in 0..8 {
            let _ = conn.flush();
            h.dispatch();
            if let Some(g) = conn.prepare_read() {
                let _ = g.read();
            }
            let _ = queue.dispatch_pending(seen);
        }
    };
    pump(&mut seen, &mut queue, &mut h);
    let find = |seen: &Seen, name: &str| {
        seen.globals
            .iter()
            .find(|(_, i, _)| i == name)
            .map(|(n, _, v)| (*n, *v))
            .unwrap_or_else(|| panic!("{name} not advertised"))
    };
    let (n, v) = find(&seen, "wl_compositor");
    let compositor: WlCompositor = registry.bind(n, v.min(5), &qh, ());
    let (n, _) = find(&seen, "wl_shm");
    let shm: wl_shm::WlShm = registry.bind(n, 1, &qh, ());
    let (n, _) = find(&seen, "xdg_wm_base");
    let wm: XdgWmBase = registry.bind(n, 1, &qh, ());
    let (n, v) = find(&seen, "wl_seat");
    let _seat: WlSeat = registry.bind(n, v.min(5), &qh, ());
    let (n, _) = find(&seen, "zwp_relative_pointer_manager_v1");
    let rel_manager: ZwpRelativePointerManagerV1 = registry.bind(n, 1, &qh, ());
    let (n, _) = find(&seen, "zwp_pointer_constraints_v1");
    let constraints: ZwpPointerConstraintsV1 = registry.bind(n, 1, &qh, ());
    pump(&mut seen, &mut queue, &mut h);

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    surface.commit();
    pump(&mut seen, &mut queue, &mut h);
    let size = WIN.0 * WIN.1 * 4;
    // SAFETY: an anonymous memfd; the name is a valid C string.
    let fd = unsafe { libc::memfd_create(c"abyss-constraints-test".as_ptr(), 0) };
    assert!(fd >= 0, "memfd_create");
    // SAFETY: `fd` was just created and nothing else owns it.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.set_len(size as u64).expect("size memfd");
    let pool = shm.create_pool(file.as_fd(), size, &qh, ());
    let buffer = pool.create_buffer(0, WIN.0, WIN.1, WIN.0 * 4, wl_shm::Format::Argb8888, &qh, ());
    pool.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    pump(&mut seen, &mut queue, &mut h);

    let pointer = seen.pointer.clone().expect("seat offered a pointer");
    let relative = rel_manager.get_relative_pointer(&pointer, &qh, ());
    let mut r = Rig {
        h,
        conn,
        queue,
        seen,
        surface,
        _xdg: xdg,
        _toplevel: toplevel,
        _relative: relative,
        constraints,
    };
    // Put the pointer in the middle of the window, as the human would.
    let at = r.origin() + Point::from((f64::from(WIN.0) / 2.0, f64::from(WIN.1) / 2.0));
    r.h.state.pointer_moved(at, 0);
    r.pump();
    assert!(r.seen.ptr_enters >= 1, "pointer focus reached the client");
    r
}

#[test]
fn device_motion_reaches_a_locked_pointer_as_relative_deltas() {
    let mut r = rig();
    let _lock = r.lock();
    let before = r.h.state.pointer_location;

    mouse(&mut r.h.state, (5.0, -3.0), (4.0, -2.0));
    mouse(&mut r.h.state, (1.5, 0.5), (1.0, 0.25));
    r.pump();

    assert_eq!(r.seen.relative, vec![(5.0, -3.0), (1.5, 0.5)]);
    assert_eq!(r.seen.relative_unaccel, vec![(4.0, -2.0), (1.0, 0.25)]);
    // Locked: the cursor itself stays put, the deltas are all the game gets.
    assert_eq!(r.h.state.pointer_location, before);
}

#[test]
fn device_motion_reaches_an_unlocked_relative_pointer_too() {
    let mut r = rig();
    mouse(&mut r.h.state, (2.0, 2.0), (2.0, 2.0));
    r.pump();
    assert_eq!(r.seen.relative, vec![(2.0, 2.0)]);
}

#[test]
fn nothing_relative_reaches_a_client_behind_the_session_lock() {
    let mut r = rig();
    r.h.state.lock.locked = true;
    mouse(&mut r.h.state, (5.0, 5.0), (5.0, 5.0));
    r.pump();
    assert!(r.seen.relative.is_empty(), "relative motion leaked past the lock");
}

#[test]
fn a_cursor_hint_is_surface_local() {
    let mut r = rig();
    let lock = r.lock();
    lock.set_cursor_position_hint(10.0, 12.0);
    r.surface.commit();
    r.pump();
    assert_eq!(
        r.h.state.pointer_location,
        r.origin() + Point::from((10.0, 12.0)),
        "the hint lands inside the window, not at the output's top-left"
    );
}
