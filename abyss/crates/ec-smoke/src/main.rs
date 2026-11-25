// SPDX-License-Identifier: AGPL-3.0-only
//! Test-only clients for the agent end-to-end smoke run (`ci/agent-smoke.sh`).
//! Never shipped. One binary, four roles:
//!
//! * `window`: an ordinary xdg toplevel. Keys and pointer buttons from any
//!   `wl_seat` it can see (the human's and, when SEAT-01 allows, an agent's)
//!   are appended to `$SMOKE_OUT`, typed text goes into the title.
//! * `slot`: a host window with a protected surface and a task-commit slot
//!   (COMP-19), standing in for the console pane.
//! * `seats`: binds every `wl_seat` and prints each one's name.
//! * `agent`: stands in for agentd's grant hand-off. Connects to `policyd.sock`,
//!   takes the `provision` grant, then drives `ec-agent.sock` (COMP-08).
//!   Must be run from a binary named `ec-agentd` (policyd's role rule).

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use ec_protocols::agent::client::{
    eclipse_agent_manager_v1::{self, EclipseAgentManagerV1},
    eclipse_agent_seat_v1::{self, EclipseAgentSeatV1},
    eclipse_agent_v1::{self, EclipseAgentV1},
    eclipse_scene_v1::{self, EclipseSceneV1},
};
use ec_protocols::protected::client::{
    eclipse_commit_slot_v1::{self, EclipseCommitSlotV1},
    eclipse_protected_surface_manager_v1::EclipseProtectedSurfaceManagerV1,
    eclipse_protected_surface_v1::{self, EclipseProtectedSurfaceV1},
};
use wayland_client::{
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_pointer::{self, WlPointer},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use xkbcommon::xkb;

fn mono_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid out pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

fn log(out: &Option<std::fs::File>, line: &str) {
    let l = format!("{} {line}\n", mono_ms());
    print!("{l}");
    let _ = std::io::stdout().flush();
    if let Some(mut f) = out.as_ref() {
        let _ = f.write_all(l.as_bytes());
    }
}

// ---------------------------------------------------------------- gui roles

struct SeatInfo {
    _seat: WlSeat,
    name: String,
    xkb: Option<xkb::State>,
}

#[derive(Default)]
struct Gui {
    out: Option<std::fs::File>,
    app_id: String,
    title: String,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm: Option<XdgWmBase>,
    prot_mgr: Option<EclipseProtectedSurfaceManagerV1>,
    surface: Option<WlSurface>,
    xdg: Option<XdgSurface>,
    toplevel: Option<XdgToplevel>,
    mapped: bool,
    size: (i32, i32),
    seats: HashMap<u32, SeatInfo>,
    typed: String,
    clicks: u32,
    slot_mode: bool,
    slot: Option<EclipseCommitSlotV1>,
    prot: Option<EclipseProtectedSurfaceV1>,
    committed: Option<String>,
    keep: Vec<WlBuffer>,
    seat_names_only: bool,
}

impl Gui {
    fn retitle(&mut self, qh: &QueueHandle<Gui>) {
        let _ = qh;
        self.title = format!("typed={} clicks={}", self.typed, self.clicks);
        if let (Some(t), Some(s)) = (&self.toplevel, &self.surface) {
            t.set_title(self.title.clone());
            s.commit();
        }
    }

    fn attach(&mut self, qh: &QueueHandle<Gui>) {
        let (w, h) = (self.size.0.max(480), self.size.1.max(240));
        let shm = self.shm.clone().unwrap();
        let stride = w * 4;
        let mut px = vec![0u8; (stride * h) as usize];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&[0x30, 0x30, 0x38, 0xff]);
        }
        let fd = rustix::fs::memfd_create("smoke-shm", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let mut f = std::fs::File::from(fd);
        f.write_all(&px).unwrap();
        let pool: WlShmPool = shm.create_pool(f.as_fd(), px.len() as i32, qh, ());
        let buf = pool.create_buffer(0, w, h, stride, wl_shm::Format::Xrgb8888, qh, ());
        let s = self.surface.as_ref().unwrap();
        s.attach(Some(&buf), 0, 0);
        s.damage_buffer(0, 0, w, h);
        s.commit();
        self.keep.push(buf);
        self.mapped = true;
    }
}

impl Dispatch<WlRegistry, ()> for Gui {
    fn event(
        g: &mut Self,
        reg: &WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = ev
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => g.compositor = Some(reg.bind(name, version.min(4), qh, ())),
            "wl_shm" => g.shm = Some(reg.bind(name, 1, qh, ())),
            "xdg_wm_base" => g.wm = Some(reg.bind(name, version.min(3), qh, ())),
            "eclipse_protected_surface_manager_v1" => {
                g.prot_mgr = Some(reg.bind(name, version.min(2), qh, ()));
            }
            "wl_seat" => {
                let seat: WlSeat = reg.bind(name, version.min(5), qh, name);
                g.seats.insert(
                    name,
                    SeatInfo {
                        _seat: seat,
                        name: String::new(),
                        xkb: None,
                    },
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, u32> for Gui {
    fn event(
        g: &mut Self,
        seat: &WlSeat,
        ev: wl_seat::Event,
        gname: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match ev {
            wl_seat::Event::Name { name } => {
                if let Some(s) = g.seats.get_mut(gname) {
                    s.name = name.clone();
                }
                log(&g.out, &format!("seat name={name}"));
            }
            wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(c),
            } if !g.seat_names_only => {
                if c.contains(wl_seat::Capability::Keyboard) {
                    seat.get_keyboard(qh, *gname);
                }
                if c.contains(wl_seat::Capability::Pointer) {
                    seat.get_pointer(qh, *gname);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, u32> for Gui {
    fn event(
        g: &mut Self,
        _: &WlKeyboard,
        ev: wl_keyboard::Event,
        gname: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let seat_name = g.seats.get(gname).map(|s| s.name.clone()).unwrap_or_default();
        match ev {
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                let mut text = String::new();
                let f = std::fs::File::from(fd);
                let mut buf = vec![0u8; size as usize];
                if std::os::unix::fs::FileExt::read_exact_at(&f, &mut buf, 0).is_ok() {
                    text = String::from_utf8_lossy(&buf).trim_end_matches('\0').to_owned();
                }
                let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                if let Some(km) = xkb::Keymap::new_from_string(
                    &ctx,
                    text,
                    xkb::KEYMAP_FORMAT_TEXT_V1,
                    xkb::COMPILE_NO_FLAGS,
                ) {
                    if let Some(s) = g.seats.get_mut(gname) {
                        s.xkb = Some(xkb::State::new(&km));
                    }
                }
                log(&g.out, &format!("keymap seat={seat_name}"));
            }
            wl_keyboard::Event::Enter { .. } => log(&g.out, &format!("kb_enter seat={seat_name}")),
            wl_keyboard::Event::Leave { .. } => log(&g.out, &format!("kb_leave seat={seat_name}")),
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(x) = g.seats.get_mut(gname).and_then(|s| s.xkb.as_mut()) {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(st),
                ..
            } => {
                let down = st == wl_keyboard::KeyState::Pressed;
                let mut ch = String::new();
                if let Some(x) = g.seats.get_mut(gname).and_then(|s| s.xkb.as_mut()) {
                    let code = xkb::Keycode::new(key + 8);
                    if down {
                        ch = x.key_get_utf8(code);
                    }
                    x.update_key(
                        code,
                        if down {
                            xkb::KeyDirection::Down
                        } else {
                            xkb::KeyDirection::Up
                        },
                    );
                }
                log(
                    &g.out,
                    &format!("key seat={seat_name} code={key} down={down} text={ch:?}"),
                );
                if down && !ch.is_empty() && !g.slot_mode {
                    g.typed.push_str(&ch);
                    g.retitle(qh);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, u32> for Gui {
    fn event(
        g: &mut Self,
        _: &WlPointer,
        ev: wl_pointer::Event,
        gname: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let seat_name = g.seats.get(gname).map(|s| s.name.clone()).unwrap_or_default();
        match ev {
            wl_pointer::Event::Enter {
                surface_x, surface_y, ..
            } => {
                log(
                    &g.out,
                    &format!("ptr_enter seat={seat_name} at={surface_x:.0},{surface_y:.0}"),
                );
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(st),
                ..
            } => {
                let down = st == wl_pointer::ButtonState::Pressed;
                log(
                    &g.out,
                    &format!("button seat={seat_name} code={button} down={down}"),
                );
                if down {
                    g.clicks += 1;
                    g.retitle(qh);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Gui {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        ev: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = ev {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Gui {
    fn event(
        g: &mut Self,
        xs: &XdgSurface,
        ev: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = ev {
            xs.ack_configure(serial);
            if !g.mapped {
                g.attach(qh);
                log(&g.out, "mapped");
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Gui {
    fn event(
        g: &mut Self,
        _: &XdgToplevel,
        ev: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                g.size = (width, height);
            }
            xdg_toplevel::Event::Close => std::process::exit(0),
            _ => {}
        }
    }
}

impl Dispatch<EclipseProtectedSurfaceV1, ()> for Gui {
    fn event(
        g: &mut Self,
        _: &EclipseProtectedSurfaceV1,
        ev: eclipse_protected_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        log(&g.out, &format!("protected_surface event {ev:?}"));
    }
}

impl Dispatch<EclipseCommitSlotV1, ()> for Gui {
    fn event(
        g: &mut Self,
        _: &EclipseCommitSlotV1,
        ev: eclipse_commit_slot_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        log(&g.out, &format!("slot event {ev:?}"));
        if let eclipse_commit_slot_v1::Event::Committed { task_id } = ev {
            g.committed = Some(task_id);
        }
    }
}

macro_rules! noop {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Gui {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
noop!(
    WlCompositor,
    WlShm,
    WlShmPool,
    WlSurface,
    EclipseProtectedSurfaceManagerV1
);

impl Dispatch<WlBuffer, ()> for Gui {
    fn event(_: &mut Self, _: &WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
    }
}

fn pump<S: 'static>(eq: &mut EventQueue<S>, st: &mut S, conn: &Connection, ms: i32) -> Result<(), String> {
    eq.dispatch_pending(st).map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;
    if let Some(guard) = eq.prepare_read() {
        let fd = guard.connection_fd().as_raw_fd_();
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let n = unsafe { libc::poll(&mut p, 1, ms) };
        if n > 0 {
            guard.read().map_err(|e| e.to_string())?;
        }
    }
    eq.dispatch_pending(st).map_err(|e| e.to_string())?;
    Ok(())
}

trait RawFdExt {
    fn as_raw_fd_(&self) -> i32;
}
impl RawFdExt for std::os::fd::BorrowedFd<'_> {
    fn as_raw_fd_(&self) -> i32 {
        std::os::fd::AsRawFd::as_raw_fd(self)
    }
}

fn gui(role: &str, args: &[String]) -> Result<(), String> {
    let out = std::env::var_os("SMOKE_OUT")
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let mut eq = conn.new_event_queue::<Gui>();
    let qh = eq.handle();
    conn.display().get_registry(&qh, ());
    let mut g = Gui {
        out,
        app_id: std::env::var("SMOKE_APP_ID").unwrap_or_else(|_| "ec-smoke-win".into()),
        slot_mode: role == "slot",
        seat_names_only: role == "seats",
        ..Gui::default()
    };
    eq.roundtrip(&mut g).map_err(|e| e.to_string())?;
    eq.roundtrip(&mut g).map_err(|e| e.to_string())?;
    if role == "seats" {
        let mut names: Vec<_> = g.seats.values().map(|s| s.name.clone()).collect();
        names.sort();
        println!("SEATS {}", names.join(","));
        return Ok(());
    }
    let (Some(comp), Some(wm)) = (g.compositor.clone(), g.wm.clone()) else {
        return Err("no wl_compositor or xdg_wm_base".into());
    };
    let surface = comp.create_surface(&qh, ());
    let xs = wm.get_xdg_surface(&surface, &qh, ());
    let tl = xs.get_toplevel(&qh, ());
    tl.set_app_id(g.app_id.clone());
    tl.set_title("typed= clicks=0".into());
    g.surface = Some(surface.clone());
    g.xdg = Some(xs);
    g.toplevel = Some(tl);
    if role == "slot" {
        let mgr = g
            .prot_mgr
            .clone()
            .ok_or("no eclipse_protected_surface_manager_v1")?;
        let prot = mgr.protect(&surface, &qh, ());
        let slot = prot.get_slot(eclipse_protected_surface_v1::Kind::TaskCommit, 20, 20, &qh, ());
        let package = args.first().cloned().unwrap_or_else(|| "smoke-agent".into());
        let statement = args
            .get(1)
            .cloned()
            .unwrap_or_else(|| "Smoke test: type hello into the window".into());
        slot.set_draft(
            package,
            statement,
            600,
            Vec::new(),
            String::new(),
            String::new(),
            0,
        );
        g.prot = Some(prot);
        g.slot = Some(slot);
    }
    surface.commit();
    let t0 = Instant::now();
    let limit = Duration::from_secs(
        std::env::var("SMOKE_LIFETIME")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120),
    );
    while t0.elapsed() < limit {
        pump(&mut eq, &mut g, &conn, 100)?;
        if g.committed.is_some() {
            log(&g.out, &format!("COMMITTED {}", g.committed.clone().unwrap()));
            // Stay up a moment so the card can be screenshotted, then leave.
            std::thread::sleep(Duration::from_millis(500));
            return Ok(());
        }
    }
    if role == "slot" {
        return Err("slot never committed".into());
    }
    Ok(())
}

// --------------------------------------------------------------- the agent

#[derive(Default)]
struct Ag {
    manager: Option<(u32, u32)>,
    results: HashMap<u32, (u32, String, u32, u32)>,
    listed: Vec<(u32, String, String, u32, u32)>, // handle, app_id, title, states, generation
    list_done: bool,
    detail_seats: Vec<String>,
    waited: HashMap<u32, (u32, u32, u32)>,
    keymap: bool,
    revoked: Vec<String>,
}

impl Dispatch<WlRegistry, ()> for Ag {
    fn event(
        a: &mut Self,
        _: &WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = ev
        {
            if interface == "eclipse_agent_manager_v1" {
                a.manager = Some((name, version));
            }
        }
    }
}
impl Dispatch<EclipseAgentManagerV1, ()> for Ag {
    fn event(
        a: &mut Self,
        _: &EclipseAgentManagerV1,
        ev: eclipse_agent_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_agent_manager_v1::Event::Revoked { principal, reason } = ev {
            a.revoked.push(format!("{principal}:{reason}"));
        }
    }
}
impl Dispatch<EclipseAgentV1, ()> for Ag {
    fn event(
        a: &mut Self,
        _: &EclipseAgentV1,
        ev: eclipse_agent_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_agent_v1::Event::Result {
            req_id,
            status,
            detail,
            focus_handle,
            generation,
            ..
        } = ev
        {
            a.results
                .insert(req_id, (status, detail, focus_handle, generation));
        }
    }
}
impl Dispatch<EclipseSceneV1, ()> for Ag {
    fn event(
        a: &mut Self,
        _: &EclipseSceneV1,
        ev: eclipse_scene_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use eclipse_scene_v1::Event as E;
        match ev {
            E::Toplevel {
                handle,
                app_id,
                title,
                states,
                generation,
                ..
            } => {
                a.listed.retain(|l| l.0 != handle);
                a.listed.push((handle, app_id, title, states, generation));
            }
            E::ToplevelsDone { .. } => a.list_done = true,
            E::ToplevelDetail { focused_by_seats, .. } => {
                a.detail_seats = focused_by_seats
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect();
            }
            E::Waited {
                req_id,
                satisfied,
                handle,
                generation,
                ..
            } => {
                a.waited.insert(req_id, (satisfied, handle, generation));
            }
            E::Result {
                req_id,
                status,
                detail,
                focus_handle,
                generation,
                ..
            } => {
                a.results
                    .insert(req_id, (status, detail, focus_handle, generation));
            }
            _ => {}
        }
    }
}
impl Dispatch<EclipseAgentSeatV1, ()> for Ag {
    fn event(
        a: &mut Self,
        _: &EclipseAgentSeatV1,
        ev: eclipse_agent_seat_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            eclipse_agent_seat_v1::Event::Result {
                req_id,
                status,
                detail,
                focus_handle,
                generation,
                ..
            } => {
                a.results
                    .insert(req_id, (status, detail, focus_handle, generation));
            }
            eclipse_agent_seat_v1::Event::Keymap { .. } => a.keymap = true,
            _ => {}
        }
    }
}

const STATUS: [&str; 26] = [
    "ok",
    "no_capability",
    "out_of_scope",
    "rate_limited",
    "stale_generation",
    "focus_lost",
    "client_gone",
    "client_timeout",
    "no_such_action",
    "sensitivity_denied",
    "policy_denied",
    "prompt_denied",
    "prompt_timeout",
    "revoked",
    "paused",
    "invalid_argument",
    "quota_exceeded",
    "duplicate",
    "deferred_timeout",
    "class_changed",
    "broker_locked",
    "secret_rotated",
    "batch_exhausted",
    "circuit_breaker",
    "provenance_required",
    "task_closed",
];

struct Run {
    conn: Connection,
    eq: EventQueue<Ag>,
    a: Ag,
    next: u32,
}

impl Run {
    fn until(&mut self, what: &str, secs: u64, f: impl Fn(&Ag) -> bool) -> Result<(), String> {
        let t0 = Instant::now();
        while !f(&self.a) {
            if t0.elapsed() > Duration::from_secs(secs) {
                return Err(format!("timed out waiting for {what}"));
            }
            pump(&mut self.eq, &mut self.a, &self.conn, 50).map_err(|e| format!("{what}: {e}"))?;
        }
        Ok(())
    }
    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
    /// Wait for one result and print the STEP line.
    fn result(&mut self, step: &str, req: u32) -> (u32, String) {
        let r = self.until(step, 15, |a| a.results.contains_key(&req));
        let (st, d, _, _) = self
            .a
            .results
            .remove(&req)
            .unwrap_or((999, format!("{r:?}"), 0, 0));
        println!(
            "STEP {step} status={} detail={d:?}",
            STATUS.get(st as usize).unwrap_or(&"none")
        );
        (st, d)
    }
}

fn agent(args: &[String]) -> Result<(), String> {
    let run_dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "XDG_RUNTIME_DIR")?;
    let sync_dir = std::env::var("SMOKE_SYNC").map_err(|_| "SMOKE_SYNC")?;
    let policyd =
        std::env::var("ECLIPSE_POLICYD_SOCKET").unwrap_or_else(|_| format!("{run_dir}/eclipse/policyd.sock"));
    let _ = args;
    // 1. the grant, from the provision policyd pushes to agentd-role peers.
    use ec_policy_eval::link::{self, FromPolicyd};
    use rustix::net::{self, RecvFlags, SocketAddrUnix, SocketFlags};
    let fd = net::socket_with(
        net::AddressFamily::UNIX,
        net::SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| e.to_string())?;
    net::connect(&fd, &SocketAddrUnix::new(&policyd).map_err(|e| e.to_string())?)
        .map_err(|e| format!("policyd.sock: {e}"))?;
    println!("STEP policyd_link connected");
    let _ = std::fs::write(format!("{sync_dir}/standin-ready"), b"1");
    let grant = loop {
        let mut buf = vec![0u8; 70_000];
        let (n, _) = net::recv(&fd, &mut buf[..], RecvFlags::empty()).map_err(|e| e.to_string())?;
        buf.truncate(n);
        if !link::is_message(&buf) {
            continue;
        }
        if let Ok(FromPolicyd::Provision {
            task,
            principal,
            grant,
            ..
        }) = FromPolicyd::decode(&buf)
        {
            println!(
                "STEP provision task={task} principal={principal} grant_bytes={}",
                grant.len()
            );
            break grant;
        }
    };
    // 2. the agent socket.
    let sock =
        std::env::var("ECLIPSE_AGENT_SOCKET").unwrap_or_else(|_| format!("{run_dir}/eclipse/ec-agent.sock"));
    let stream = UnixStream::connect(&sock).map_err(|e| format!("ec-agent.sock: {e}"))?;
    let conn = Connection::from_socket(stream).map_err(|e| e.to_string())?;
    let eq = conn.new_event_queue::<Ag>();
    let qh = eq.handle();
    let reg = conn.display().get_registry(&qh, ());
    let mut r = Run {
        conn,
        eq,
        a: Ag::default(),
        next: 100,
    };
    r.eq.roundtrip(&mut r.a).map_err(|e| e.to_string())?;
    let (name, ver) = r.a.manager.ok_or("no eclipse_agent_manager_v1")?;
    let mgr: EclipseAgentManagerV1 = reg.bind(name, ver.min(3), &qh, ());
    let agent = mgr.create_agent(grant, &qh, ());
    let scene = agent.get_scene(&qh, ());
    match r.eq.roundtrip(&mut r.a) {
        Ok(_) => println!("STEP create_agent ok"),
        Err(e) => return Err(format!("STEP create_agent FAILED: {e}")),
    }
    // 3. wait for the window, then list.
    let id = r.id();
    scene.wait_for(id, "toplevel_appears app_id=\"ec-smoke-win\"".into(), 2_000);
    r.until("window", 35, |a| {
        a.waited.contains_key(&id) || a.results.contains_key(&id)
    })?;
    println!("STEP wait_toplevel_appears {:?}", r.a.waited.get(&id).or(None));
    let id = r.id();
    scene.list_toplevels(id, String::new());
    r.until("list", 10, |a| a.list_done || a.results.contains_key(&id))?;
    for l in &r.a.listed {
        println!(
            "STEP listed handle={} app_id={} title={:?} states={} gen={}",
            l.0, l.1, l.2, l.3, l.4
        );
    }
    let Some(win) =
        r.a.listed
            .iter()
            .find(|l| l.1 == "ec-smoke-win")
            .map(|l| (l.0, l.4))
    else {
        return Err("window not listed".into());
    };
    let h = win.0;
    println!("STEP only_in_scope_listed count={}", r.a.listed.len());
    // 4. the seat.
    let seat = agent.get_seat(&qh, ());
    r.eq.roundtrip(&mut r.a).map_err(|e| e.to_string())?;
    println!("STEP get_seat keymap={}", r.a.keymap);
    // 5. focus.
    let id = r.id();
    seat.focus(id, h, 0, 0, Vec::new());
    r.result("focus", id);
    let id = r.id();
    scene.get_toplevel(id, h);
    let _ = r.until("detail", 5, |a| {
        !a.detail_seats.is_empty() || a.results.contains_key(&id)
    });
    println!(
        "STEP focused_by_seats {:?} {:?}",
        r.a.detail_seats,
        r.a.results.remove(&id)
    );
    // 6. text, then see it in the title.
    let id = r.id();
    seat.text(id, h, 0, "hello".into(), 0, 0, Vec::new());
    r.result("text", id);
    let id = r.id();
    scene.wait_for(
        id,
        format!("title_matches handle={h} regex=\"typed=hello\""),
        4000,
    );
    let _ = r.until("title wait", 6, |a| {
        a.waited.contains_key(&id) || a.results.contains_key(&id)
    });
    println!(
        "STEP wait_title_hello {:?} {:?}",
        r.a.waited.get(&id),
        r.a.results.remove(&id)
    );
    // 7. click.
    let id = r.id();
    seat.click(id, h, 0, 60, 60, 0x110, 0, 0, Vec::new());
    r.result("click", id);
    let id = r.id();
    scene.wait_for(id, format!("title_matches handle={h} regex=\"clicks=1\""), 4000);
    let _ = r.until("click wait", 6, |a| {
        a.waited.contains_key(&id) || a.results.contains_key(&id)
    });
    println!(
        "STEP wait_title_clicks {:?} {:?}",
        r.a.waited.get(&id),
        r.a.results.remove(&id)
    );
    // 8. compat lock; the script plays the human meanwhile.
    let id = r.id();
    seat.compat_lock(id, h, 30_000);
    let (st, _) = r.result("compat_lock", id);
    if st == 0 {
        println!("LOCKED");
        let _ = std::fs::write(format!("{sync_dir}/locked"), mono_ms().to_string());
        let id = r.id();
        seat.text(id, h, 0, "X".into(), 0, 0, Vec::new());
        r.result("text_during_lock", id);
        let t0 = Instant::now();
        while !std::path::Path::new(&format!("{sync_dir}/human-sent")).exists()
            && t0.elapsed() < Duration::from_secs(40)
        {
            let _ = pump(&mut r.eq, &mut r.a, &r.conn, 50);
        }
        let _ = std::fs::write(format!("{sync_dir}/unlocking"), mono_ms().to_string());
        let id = r.id();
        seat.compat_unlock(id);
        r.result("compat_unlock", id);
        let _ = std::fs::write(format!("{sync_dir}/unlocked"), mono_ms().to_string());
    }
    // 9. wait_for focus.
    let id = r.id();
    scene.wait_for(id, format!("focus seat=\"human\" handle={h}"), 3000);
    let _ = r.until("focus wait", 5, |a| {
        a.waited.contains_key(&id) || a.results.contains_key(&id)
    });
    println!(
        "STEP wait_focus_human {:?} {:?}",
        r.a.waited.get(&id),
        r.a.results.remove(&id)
    );
    let id = r.id();
    scene.wait_for(
        id,
        format!("title_matches handle={h} regex=\"typed=helloX\""),
        4000,
    );
    let _ = r.until("title wait2", 6, |a| {
        a.waited.contains_key(&id) || a.results.contains_key(&id)
    });
    println!(
        "STEP wait_title_helloX {:?} {:?}",
        r.a.waited.get(&id),
        r.a.results.remove(&id)
    );
    // 10. out of scope: a capability the grant never named, and handles that are not ours.
    let id = r.id();
    seat.touch_down(id, 0, 10, 10, 0, Vec::new());
    r.result("touch_down_ungranted", id);
    for bogus in [h + 1, h + 2, 1, 2, 3] {
        if bogus == h {
            continue;
        }
        let id = r.id();
        seat.focus(id, bogus, 0, 0, Vec::new());
        r.result(&format!("focus_other_handle_{bogus}"), id);
    }
    let id = r.id();
    seat.focus(id, h, 1, 0, Vec::new());
    r.result("focus_stale_generation", id);
    println!("STEP done");
    let _ = std::fs::write(format!("{sync_dir}/agent-done"), b"1");
    // Stay connected so the audit trail has a live principal until killed.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20)
        && !std::path::Path::new(&format!("{sync_dir}/stop")).exists()
    {
        let _ = pump(&mut r.eq, &mut r.a, &r.conn, 100);
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(role) = args.get(1).cloned() else {
        eprintln!("usage: ec-smoke window|slot [package statement]|seats|agent");
        std::process::exit(2);
    };
    let r = match role.as_str() {
        "window" | "slot" | "seats" => gui(&role, &args[2..]),
        "agent" => agent(&args[2..]),
        _ => Err("unknown role".into()),
    };
    if let Err(e) = r {
        eprintln!("ec-smoke {role}: {e}");
        std::process::exit(1);
    }
}
