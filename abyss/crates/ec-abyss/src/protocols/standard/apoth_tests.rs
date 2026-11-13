// SPDX-License-Identifier: AGPL-3.0-only
//! X3/X4 protocols against the in-process harness: xdg-system-bell,
//! xdg-toplevel-icon, keyboard-shortcuts-inhibit, fifo, commit-timing.

use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;

use smithay::backend::input::KeyState;
use smithay::input::keyboard::Keycode;
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::wp::commit_timing::v1::client::{
    wp_commit_timer_v1::WpCommitTimerV1, wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
};
use wayland_protocols::wp::fifo::v1::client::{wp_fifo_manager_v1::WpFifoManagerV1, wp_fifo_v1::WpFifoV1};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1, xdg_toplevel_icon_v1::XdgToplevelIconV1,
};

use crate::input::{Action, Bind, Mods};
use crate::shell::focus::state_tests::{harness, Harness};

#[derive(Default)]
struct Seen {
    globals: Vec<(u32, String, u32)>,
    closes: u32,
    inhibitor_active: u32,
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
impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &XdgToplevel,
        e: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = e {
            s.closes += 1;
        }
    }
}
impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        e: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_keyboard_shortcuts_inhibitor_v1::Event::Active = e {
            s.inhibitor_active += 1;
        }
    }
}
impl Dispatch<XdgToplevelIconManagerV1, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgToplevelIconManagerV1,
        _: <XdgToplevelIconManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore wayland_client::protocol::wl_seat::WlSeat);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore wl_shm::WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore XdgSystemBellV1);
delegate_noop!(Seen: ignore XdgToplevelIconV1);
delegate_noop!(Seen: ignore ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(Seen: ignore WpFifoManagerV1);
delegate_noop!(Seen: ignore WpFifoV1);
delegate_noop!(Seen: ignore WpCommitTimingManagerV1);
delegate_noop!(Seen: ignore WpCommitTimerV1);

struct Rig {
    h: Harness,
    conn: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: WlSurface,
    registry: wl_registry::WlRegistry,
    shm_buf: WlBuffer,
    _xdg: XdgSurface,
    _toplevel: XdgToplevel,
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

    fn bind<I: wayland_client::Proxy + 'static>(&mut self, name: &str, max: u32) -> I
    where
        Seen: Dispatch<I, ()>,
    {
        let (n, v, _) = self
            .seen
            .globals
            .iter()
            .find(|(_, i, _)| i == name)
            .map(|(n, _, v)| (*n, (*v).min(max), ()))
            .unwrap_or_else(|| panic!("{name} not advertised"));
        let qh = self.queue.handle();
        let o = self.registry.bind(n, v, &qh, ());
        self.pump();
        o
    }

    fn server_surface(&self) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        self.h.state.xdg_shell_state.toplevel_surfaces()[0]
            .wl_surface()
            .clone()
    }

    fn scale_of_current(&self) -> i32 {
        use smithay::wayland::compositor::{with_states, SurfaceAttributes};
        let s = self.server_surface();
        with_states(&s, |st| {
            st.cached_state.get::<SurfaceAttributes>().current().buffer_scale
        })
    }

    fn output(&self) -> smithay::output::Output {
        self.h.state.outputs.iter().next().expect("output").output.clone()
    }
}

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
    for _ in 0..8 {
        let _ = conn.flush();
        h.dispatch();
        if let Some(g) = conn.prepare_read() {
            let _ = g.read();
        }
        let _ = queue.dispatch_pending(&mut seen);
    }
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
    let _seat: wayland_client::protocol::wl_seat::WlSeat = registry.bind(n, v.min(5), &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    surface.commit();
    let size = 300 * 200 * 4;
    // SAFETY: anonymous memfd with a valid C name.
    let fd = unsafe { libc::memfd_create(c"abyss-apoth-test".as_ptr(), 0) };
    assert!(fd >= 0);
    // SAFETY: freshly created, solely owned.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.set_len(size as u64).unwrap();
    let pool = shm.create_pool(file.as_fd(), size, &qh, ());
    let buf = pool.create_buffer(0, 300, 200, 1200, wl_shm::Format::Argb8888, &qh, ());
    pool.destroy();
    let mut r = Rig {
        h,
        conn,
        queue,
        seen,
        surface,
        registry,
        shm_buf: buf,
        _xdg: xdg,
        _toplevel: toplevel,
    };
    r.pump();
    r.surface.attach(Some(&r.shm_buf), 0, 0);
    r.surface.commit();
    r.pump();
    r
}

const KEY_LEFTMETA: u32 = 125;
const KEY_A: u32 = 30;

fn press_super_a(r: &mut Rig) {
    for (code, st) in [
        (KEY_LEFTMETA, KeyState::Pressed),
        (KEY_A, KeyState::Pressed),
        (KEY_A, KeyState::Released),
        (KEY_LEFTMETA, KeyState::Released),
    ] {
        r.h.state.keyboard_key(Keycode::new(code + 8), st, 1);
    }
    r.pump();
}

#[test]
fn all_five_protocols_are_advertised() {
    let r = rig();
    for name in [
        "xdg_system_bell_v1",
        "xdg_toplevel_icon_manager_v1",
        "zwp_keyboard_shortcuts_inhibit_manager_v1",
        "wp_fifo_manager_v1",
        "wp_commit_timing_manager_v1",
    ] {
        assert!(r.seen.globals.iter().any(|(_, i, _)| i == name), "{name} missing");
    }
}

#[test]
fn bell_emits_a_window_event() {
    let mut r = rig();
    let bell: XdgSystemBellV1 = r.bind("xdg_system_bell_v1", 1);
    let _ = crate::ipc::capture::take();
    bell.ring(Some(&r.surface));
    r.pump();
    let ev = crate::ipc::capture::take();
    assert!(
        ev.iter().any(|(k, p)| k == "window" && p["change"] == "bell"),
        "no bell event in {ev:?}"
    );
}

#[test]
fn toplevel_icon_name_is_latched_on_commit() {
    let mut r = rig();
    let mgr: XdgToplevelIconManagerV1 = r.bind("xdg_toplevel_icon_manager_v1", 1);
    let qh = r.queue.handle();
    let icon = mgr.create_icon(&qh, ());
    icon.set_name("org.example.app".into());
    mgr.set_icon(&r._toplevel, Some(&icon));
    r.pump();
    assert_eq!(
        super::toplevel_icon::icon_name(&r.server_surface()),
        None,
        "double buffered"
    );
    r.surface.commit();
    r.pump();
    assert_eq!(
        super::toplevel_icon::icon_name(&r.server_surface()).as_deref(),
        Some("org.example.app")
    );
}

#[test]
fn inhibitor_passes_bindings_but_not_the_reserved_chord() {
    let mut r = rig();
    r.h.state.config.binds.insert(
        0,
        Bind {
            mods: Mods {
                logo: true,
                ..Default::default()
            },
            key: smithay::input::keyboard::Keysym::a,
            action: Action::Close,
        },
    );
    press_super_a(&mut r);
    assert_eq!(r.seen.closes, 1, "binding runs with no inhibitor");

    let mgr: ZwpKeyboardShortcutsInhibitManagerV1 = r.bind("zwp_keyboard_shortcuts_inhibit_manager_v1", 1);
    let qh = r.queue.handle();
    let wl_seat: wayland_client::protocol::wl_seat::WlSeat = r.bind("wl_seat", 5);
    let _inh = mgr.inhibit_shortcuts(&r.surface, &wl_seat, &qh, ());
    r.pump();
    assert_eq!(r.seen.inhibitor_active, 1, "honoured");
    assert!(super::shortcuts_inhibit::focus_inhibits(&r.h.state));
    press_super_a(&mut r);
    assert_eq!(r.seen.closes, 1, "binding went to the client instead");
    assert!(super::shortcuts_inhibit::survives_inhibit(&Action::AgentOverride));
}

#[test]
fn fifo_and_commit_timing_hold_commits_until_released() {
    let mut r = rig();
    let fm: WpFifoManagerV1 = r.bind("wp_fifo_manager_v1", 1);
    let tm: WpCommitTimingManagerV1 = r.bind("wp_commit_timing_manager_v1", 1);
    let qh = r.queue.handle();
    let fifo = fm.get_fifo(&r.surface, &qh, ());
    fifo.set_barrier();
    r.surface.commit();
    r.pump();
    fifo.wait_barrier();
    r.surface.set_buffer_scale(2);
    r.surface.commit();
    r.pump();
    assert_eq!(r.scale_of_current(), 1, "held behind the barrier");
    let out = r.output();
    super::frame_timing::release(&mut r.h.state, &out);
    r.pump();
    assert_eq!(r.scale_of_current(), 2, "released by the frame");

    // Commit timing: a target an hour away stays held and arms a wake-up.
    let timer = tm.get_timer(&r.surface, &qh, ());
    let now = r.h.state.clock.now();
    let secs: u64 = std::time::Duration::from(now).as_secs() + 3600;
    timer.set_timestamp((secs >> 32) as u32, secs as u32, 0);
    r.surface.set_buffer_scale(3);
    r.surface.commit();
    r.pump();
    super::frame_timing::release(&mut r.h.state, &out);
    r.pump();
    assert_eq!(r.scale_of_current(), 2, "target is in the future");
    assert!(r.h.state.timing_wakeup_armed);
}
