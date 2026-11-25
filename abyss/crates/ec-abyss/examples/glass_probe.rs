// SPDX-License-Identifier: AGPL-3.0-only
//! Headless glass probe (ADR 0130): a transparent layer surface at the output's
//! top-left holding two overlapping `eclipse_glass_v1` panes, a bar and a drawer
//! hanging off its bottom-right corner, joined or not. It exists for the
//! screenshot checks of joined panes; it is a test client, not a shipped one.
//!
//! The compositor admits the glass global only to allowlisted exe names, so
//! run the built binary copied to an allowlisted name (for example `ec-toasts`)
//! inside a headless session:
//!
//! ```text
//! glass_probe <none|plain|joined> <seconds>
//! ```
//!
//! `none` sets no panes (the bare wallpaper), `plain` two panes that stand
//! alone, `joined` the same two with one `set_joins` of radius 16.

use std::{
    fs::File,
    io::Write,
    os::fd::{AsFd, FromRawFd, OwnedFd},
    time::Duration,
};

use ec_protocols::glass::{
    client::{
        eclipse_glass_manager_v1::EclipseGlassManagerV1, eclipse_glass_surface_v1::EclipseGlassSurfaceV1,
    },
    encode_joins, JoinRecord,
};
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

/// The layer surface's size, logical px, at the output's top-left.
const W: u32 = 600;
const H: u32 = 300;

#[derive(Default)]
struct App {
    configured: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        proxy: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            proxy.ack_configure(serial);
            state.configured = true;
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(App: ignore EclipseGlassSurfaceV1);
delegate_noop!(App: ignore EclipseGlassManagerV1);

/// One pane record: x y w h radius bezel tone state alpha, native-endian u32s.
fn pane(x: u32, y: u32, w: u32, h: u32, radius: u32) -> Vec<u8> {
    [x, y, w, h, radius, 0, 0, 0, 255]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect()
}

/// A transparent memfd-backed buffer of `W` x `H` ARGB pixels.
fn transparent_buffer(shm: &wl_shm::WlShm, qh: &QueueHandle<App>) -> (wl_buffer::WlBuffer, File) {
    let len = (W * H * 4) as usize;
    // SAFETY: memfd_create with a constant name and flags; the fd is owned below.
    let fd = unsafe { libc::memfd_create(c"glass-probe".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    // SAFETY: `fd` is a fresh descriptor from memfd_create, owned by nothing else.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let file = File::from(owned);
    file.set_len(len as u64).expect("size the buffer");
    let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        W as i32,
        H as i32,
        (W * 4) as i32,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    (buffer, file)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "joined".into());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let conn = Connection::connect_to_env().expect("WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=5, ()).expect("wl_compositor");
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).expect("layer shell");
    let manager: EclipseGlassManagerV1 = globals.bind(&qh, 1..=3, ()).expect("eclipse_glass_manager_v1");

    let surface = compositor.create_surface(&qh, ());
    let layer = shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Top,
        "glass-probe".into(),
        &qh,
        (),
    );
    layer.set_anchor(zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left);
    layer.set_size(W, H);
    layer.set_exclusive_zone(-1);
    surface.commit();
    let mut app = App::default();
    while !app.configured {
        queue.blocking_dispatch(&mut app).expect("dispatch");
    }

    let (buffer, _file) = transparent_buffer(&shm, &qh);
    surface.attach(Some(&buffer), 0, 0);
    let glass: EclipseGlassSurfaceV1 = manager.get_glass(&surface, &qh, ());
    // The bar: a pill; the drawer hangs from its bottom-right corner, its left
    // edge 50 px in from the pill's right end, so the inside corner is at (290, 80).
    let panes = match mode.as_str() {
        "none" => Vec::new(),
        _ => [pane(40, 40, 300, 40, 12), pane(290, 80, 50, 160, 12)].concat(),
    };
    glass.set_panes(panes);
    if mode == "joined" {
        glass.set_joins(encode_joins(&[JoinRecord {
            a: 0,
            b: 1,
            radius: 16,
        }]));
    }
    surface.damage_buffer(0, 0, W as i32, H as i32);
    surface.commit();
    queue.roundtrip(&mut app).expect("roundtrip");
    println!("glass-probe {mode} up for {seconds}s");
    std::io::stdout().flush().ok();
    let end = std::time::Instant::now() + Duration::from_secs(seconds);
    while std::time::Instant::now() < end {
        queue.dispatch_pending(&mut app).expect("dispatch");
        std::thread::sleep(Duration::from_millis(100));
    }
}
