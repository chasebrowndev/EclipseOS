// SPDX-License-Identifier: AGPL-3.0-only
//! The reference client for `eclipse_semantic_v1` (COMP-09 §6, COMP-16 M22):
//! a real `wayland-client` on the in-process harness. It maps an xdg
//! toplevel, attaches a semantic surface and forwards a
//! [`ec_cataclysm_pub::Batch`] to the wire verbatim, which is exactly what
//! the foot fork does over the C ABI (ADR 0034): the publisher decides the
//! requests, the client only sends them.
//!
//! Test-only. A client for a toolkit bridge lives with its toolkit (P-03).

use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;

use ec_cataclysm_pub::semantic::OpKind;
use ec_cataclysm_pub::Batch;
use ec_protocols::semantic::client::{
    eclipse_semantic_manager_v1::{self, EclipseSemanticManagerV1},
    eclipse_semantic_surface_v1::{self, EclipseSemanticSurfaceV1},
};
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use crate::shell::focus::state_tests::Harness;

/// Buffer size of a mapped window.
pub const WIN: (i32, i32) = (200, 100);

/// What the compositor has told the client.
#[derive(Default)]
pub struct Seen {
    globals: Vec<(u32, String, u32)>,
    pub caps: Option<u32>,
    /// `action_requested`: (serial, node, verb, args).
    pub actions: Vec<(u32, u32, u32, String)>,
    pub budget: usize,
    pub hints: Vec<u32>,
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

impl Dispatch<EclipseSemanticManagerV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseSemanticManagerV1,
        e: eclipse_semantic_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_semantic_manager_v1::Event::Capabilities { flags } = e {
            s.caps = Some(flags);
        }
    }
}

impl Dispatch<EclipseSemanticSurfaceV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseSemanticSurfaceV1,
        e: eclipse_semantic_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use eclipse_semantic_surface_v1::Event;
        match e {
            Event::ActionRequested {
                serial,
                node,
                verb,
                args,
            } => s.actions.push((serial, node, verb, args)),
            Event::FocusHint { node } => s.hints.push(node),
            Event::BudgetExceeded => s.budget += 1,
            _ => {}
        }
    }
}

delegate_noop!(Seen: WlCompositor);
delegate_noop!(Seen: WlShmPool);
delegate_noop!(Seen: ignore wl_shm::WlShm);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore XdgToplevel);

/// A mapped toplevel's client objects.
pub struct Win {
    pub surface: WlSurface,
    pub toplevel: XdgToplevel,
    _xdg: XdgSurface,
}

pub struct Ref {
    conn: Connection,
    queue: EventQueue<Seen>,
    pub seen: Seen,
    compositor: WlCompositor,
    shm: wl_shm::WlShm,
    wm: XdgWmBase,
    pub manager: EclipseSemanticManagerV1,
}

impl Ref {
    pub fn connect(h: &mut Harness) -> Self {
        let (server, client) = UnixStream::pair().expect("socket pair");
        h.state
            .display_handle
            .insert_client(server, crate::state::client_state())
            .expect("insert client");
        let conn = Connection::from_socket(client).expect("connect");
        let queue = conn.new_event_queue();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        let mut seen = Seen::default();
        let mut q = queue;
        // Two round trips: the globals, then the binds' own events.
        let mut pump = |seen: &mut Seen, q: &mut EventQueue<Seen>| {
            for _ in 0..8 {
                let _ = conn.flush();
                h.dispatch();
                if let Some(g) = conn.prepare_read() {
                    let _ = g.read();
                }
                let _ = q.dispatch_pending(seen);
            }
        };
        pump(&mut seen, &mut q);
        let find = |seen: &Seen, name: &str| {
            seen.globals
                .iter()
                .find(|(_, i, _)| i == name)
                .map(|(n, _, v)| (*n, *v))
                .unwrap_or_else(|| panic!("{name} not advertised"))
        };
        let (n, v) = find(&seen, "wl_compositor");
        let compositor = registry.bind(n, v.min(5), &qh, ());
        let (n, _) = find(&seen, "wl_shm");
        let shm = registry.bind(n, 1, &qh, ());
        let (n, _) = find(&seen, "xdg_wm_base");
        let wm = registry.bind(n, 1, &qh, ());
        let (n, v) = find(&seen, "eclipse_semantic_manager_v1");
        assert_eq!(v, 1);
        let manager = registry.bind(n, 1, &qh, ());
        pump(&mut seen, &mut q);
        Self {
            conn,
            queue: q,
            seen,
            compositor,
            shm,
            wm,
            manager,
        }
    }

    /// Run both ends until the traffic settles.
    pub fn pump(&mut self, h: &mut Harness) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            h.dispatch();
            if let Some(g) = self.conn.prepare_read() {
                let _ = g.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.seen);
        }
    }

    /// The protocol error the compositor killed this client with, as its
    /// code, if any.
    pub fn error(&self) -> Option<u32> {
        self.conn.protocol_error().map(|e| e.code)
    }

    fn buffer(&self, w: i32, h: i32) -> WlBuffer {
        let size = w * h * 4;
        // SAFETY: an anonymous memfd; the name is a valid C string.
        let fd = unsafe { libc::memfd_create(c"abyss-semantic-test".as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create");
        // SAFETY: `fd` was just created and nothing else owns it.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.set_len(size as u64).expect("size memfd");
        let qh = self.queue.handle();
        let pool = self.shm.create_pool(file.as_fd(), size, &qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        buffer
    }

    /// An xdg toplevel with a [`WIN`]-sized buffer, mapped.
    pub fn map(&mut self, h: &mut Harness) -> Win {
        let qh = self.queue.handle();
        let surface = self.compositor.create_surface(&qh, ());
        let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        surface.commit();
        self.pump(h);
        surface.attach(Some(&self.buffer(WIN.0, WIN.1)), 0, 0);
        surface.commit();
        self.pump(h);
        Win {
            surface,
            toplevel,
            _xdg: xdg,
        }
    }

    /// Attach a semantic surface to `win`'s toplevel.
    pub fn semantic(&mut self, h: &mut Harness, win: &Win) -> EclipseSemanticSurfaceV1 {
        let qh = self.queue.handle();
        let sem = self.manager.get_semantic_surface(&win.toplevel, &qh, ());
        self.pump(h);
        sem
    }

    /// Resize the window's buffer, which is what a resize looks like to the
    /// capture path: the surface's size changes, the tree does not.
    pub fn resize(&mut self, h: &mut Harness, win: &Win, w: i32, hh: i32) {
        win.surface.attach(Some(&self.buffer(w, hh)), 0, 0);
        win.surface.commit();
        self.pump(h);
    }
}

/// Send `batch` as the requests it is, in order, ending in its `commit`.
/// Nothing is decided here (ADR 0034).
pub fn forward(sem: &EclipseSemanticSurfaceV1, batch: &Batch<'_>) {
    for op in batch.ops() {
        match op.kind {
            OpKind::SetRoot => sem.set_root(op.node),
            OpKind::AddNode => sem.add_node(op.node, op.parent, op.index),
            OpKind::RemoveNode => sem.remove_node(op.node),
            OpKind::MoveNode => sem.move_node(op.node, op.parent, op.index),
            OpKind::SetRole => sem.set_role(op.node, op.role),
            OpKind::SetValueText => sem.set_value_text(
                op.node,
                batch.str_of(op).unwrap_or("").to_owned(),
                op.cursor,
                op.sel_start,
                op.sel_end,
            ),
            OpKind::SetExt => sem.set_ext(
                op.node,
                op.key
                    .map(|k| k.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                batch.str_of(op).unwrap_or("").to_owned(),
            ),
            OpKind::Commit => sem.commit(),
        }
    }
}

/// `u32` words as a Wayland array.
pub fn array(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_ne_bytes()).collect()
}
