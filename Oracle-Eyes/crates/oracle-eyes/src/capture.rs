// SPDX-License-Identifier: AGPL-3.0-only

//! Pixels, over the Wayland path (spec §4) — `ext-image-copy-capture-v1`.
//!
//! Capture is the half of Oracle-Eyes that is *not* on the control socket,
//! deliberately: "may draw but may no longer read" has to be a reachable
//! state, so the two grants travel over two different transports and are
//! revoked independently. The compositor hides both capture globals from a
//! client the allowlist does not name, so an absent global is not a build
//! problem — it is the answer "you have no capture capability", and
//! [`Capturer::connect`] says exactly that.
//!
//! # Protocol flow
//!
//! One grab is five round trips, all bounded by a deadline:
//!
//! 1. bind `wl_shm`, `wl_output`,
//!    `ext_output_image_capture_source_manager_v1` and
//!    `ext_image_copy_capture_manager_v1` from the registry, then learn each
//!    output's position, mode and scale from `wl_output`;
//!
//! Logical geometry comes from `wl_output` alone — `xdg-output` lives behind
//! the `unstable` feature of `wayland-protocols`, which this crate does not
//! enable, so the logical size is the mode divided by the integer scale. That
//! is exact under integer scaling and approximate under fractional scaling;
//! the crop rounds outward, so the approximation costs at most a pixel of
//! extra context rather than a missing glyph edge.
//! 2. pick the output whose logical rectangle contains the region's top-left
//!    and turn it into an `ext_image_capture_source_v1`;
//! 3. `create_session` on it, and wait for the `buffer_size` / `shm_format`
//!    pair that `done` terminates — the compositor dictates the buffer, the
//!    client does not ask;
//! 4. allocate exactly that buffer in an anonymous memfd, `create_frame`,
//!    `attach_buffer`, `capture`, and wait for `ready` or `failed`;
//! 5. convert the whole-output buffer to RGBA8 while cropping it to the
//!    requested rectangle, and tear the session down.
//!
//! The source is always an *output*: the compositor implements only the
//! output source manager (no foreign-toplevel one), and capture sessions are
//! whole-output by construction — the protocol has no crop request, so the
//! cropping is ours.
//!
//! Nothing here is long-lived. A session left open makes the compositor
//! composite a capture pass every 16 ms whether or not anyone reads it, and
//! it keeps the capture indicator lit; Phase 1 is single-shot, so every
//! object created in [`Capturer::grab`] dies before it returns.

// Phase 1's `main.rs` has no capture capability wired up yet (see its module
// doc), so nothing calls this module from the binary and every item in it
// reads as dead. Drop this the moment the select-mode path lands.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use crate::frame::{Frame, Region};
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_output::{self, Transform, WlOutput},
    wl_registry::WlRegistry,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

/// How long the initial registry/geometry exchange may take. Generous
/// relative to a local socket; the point is that a compositor which accepts
/// the connection and then never answers fails as an error, not as a hang.
const SETUP_TIMEOUT: Duration = Duration::from_secs(2);
/// How long one frame may take. The compositor services captures on a ~16 ms
/// timer, so this is several hundred frames of slack.
const GRAB_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Pure helpers — everything here is testable without a compositor, which is
// why the maths lives in free functions rather than inside `grab`.
// ---------------------------------------------------------------------------

/// The shm formats we can read.
///
/// `wl_shm` format names describe a 32-bit **little-endian word**, not a byte
/// sequence, so the memory order is the name reversed: `Xbgr8888` is
/// `0xXXBBGGRR`, i.e. bytes `R,G,B,X` — already RGBA8 byte order, which is
/// why the compositor picked it (it falls straight out of a GL `RGBA`
/// readback). `Xrgb8888` is `0xXXRRGGBB`, i.e. bytes `B,G,R,X`, and needs the
/// red and blue channels swapped. Getting this backwards is invisible in
/// greyscale text and catastrophic in OCR of coloured UI, so it is tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PixFmt {
    /// Bytes are R,G,B,A.
    Rgba,
    /// Bytes are B,G,R,A.
    Bgra,
}

impl PixFmt {
    fn from_shm(f: wl_shm::Format) -> Option<PixFmt> {
        match f {
            wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(PixFmt::Rgba),
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(PixFmt::Bgra),
            _ => None,
        }
    }
}

/// Everything about one output that the crop maths needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputGeom {
    /// Logical rectangle in the compositor's global coordinate space — the
    /// space `Region` is expressed in.
    logical: Region,
    /// Capture buffer size in physical pixels, i.e. the output's mode.
    buf_w: i32,
    buf_h: i32,
}

/// Index of the output whose logical rectangle contains `region`'s top-left.
///
/// Top-left rather than "most overlap": a selection is anchored where the
/// user started dragging, and a region straddling two outputs then crops to
/// the part on the output they meant. Half an answer beats a wrong one.
fn pick_output(outs: &[OutputGeom], region: Region) -> Option<usize> {
    outs.iter().position(|o| {
        region.x >= o.logical.x
            && region.y >= o.logical.y
            && region.x < o.logical.x.saturating_add(o.logical.w)
            && region.y < o.logical.y.saturating_add(o.logical.h)
    })
}

/// A rectangle in buffer (physical pixel) coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Map a logical rectangle onto the output's capture buffer, clamped to it.
///
/// Scale is derived from the buffer/logical ratio rather than from
/// `wl_output.scale`, because fractional scaling makes the integer scale a
/// lie while the ratio stays honest. Rounding is outward — an OCR pass would
/// rather see one extra pixel column than half a glyph. Returns `None` if
/// nothing of the region lands on the output.
fn logical_to_buffer(region: Region, out: &OutputGeom) -> Option<PixRect> {
    if out.logical.w <= 0 || out.logical.h <= 0 || out.buf_w <= 0 || out.buf_h <= 0 {
        return None;
    }
    let sx = f64::from(out.buf_w) / f64::from(out.logical.w);
    let sy = f64::from(out.buf_h) / f64::from(out.logical.h);
    let rx = f64::from(region.x - out.logical.x) * sx;
    let ry = f64::from(region.y - out.logical.y) * sy;
    let rw = f64::from(region.w.max(0)) * sx;
    let rh = f64::from(region.h.max(0)) * sy;

    let x0 = (rx.floor() as i64).clamp(0, i64::from(out.buf_w));
    let y0 = (ry.floor() as i64).clamp(0, i64::from(out.buf_h));
    let x1 = ((rx + rw).ceil() as i64).clamp(0, i64::from(out.buf_w));
    let y1 = ((ry + rh).ceil() as i64).clamp(0, i64::from(out.buf_h));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixRect {
        x: x0 as i32,
        y: y0 as i32,
        w: (x1 - x0) as i32,
        h: (y1 - y0) as i32,
    })
}

/// The logical rectangle that `crop` actually covers, so `Frame.origin`
/// describes the pixels handed back rather than the pixels asked for. OCR
/// boxes are mapped back through this to produce an anchor.
fn buffer_to_logical(crop: PixRect, out: &OutputGeom) -> Region {
    let sx = f64::from(out.logical.w) / f64::from(out.buf_w.max(1));
    let sy = f64::from(out.logical.h) / f64::from(out.buf_h.max(1));
    Region {
        x: out.logical.x + (f64::from(crop.x) * sx).round() as i32,
        y: out.logical.y + (f64::from(crop.y) * sy).round() as i32,
        w: (f64::from(crop.w) * sx).round().max(1.0) as i32,
        h: (f64::from(crop.h) * sy).round().max(1.0) as i32,
    }
}

/// Crop and convert in one pass: the source buffer is a whole output, and
/// touching only the cropped rows keeps a 4K grab from copying 33 MB to throw
/// most of it away.
fn crop_convert(
    src: &[u8],
    src_stride: usize,
    fmt: PixFmt,
    crop: PixRect,
) -> Result<Vec<u8>, String> {
    let w = crop.w.max(0) as usize;
    let h = crop.h.max(0) as usize;
    let x = crop.x.max(0) as usize;
    let y = crop.y.max(0) as usize;
    let needed = (y + h).saturating_sub(1) * src_stride + (x + w) * 4;
    if src.len() < needed {
        return Err(format!(
            "capture buffer is {} bytes, crop needs {needed}",
            src.len()
        ));
    }
    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        let s = (y + row) * src_stride + x * 4;
        let d = row * w * 4;
        let line = &src[s..s + w * 4];
        let dst = &mut out[d..d + w * 4];
        match fmt {
            PixFmt::Rgba => dst.copy_from_slice(line),
            PixFmt::Bgra => {
                for (px, dp) in line.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                    dp[0] = px[2];
                    dp[1] = px[1];
                    dp[2] = px[0];
                    dp[3] = px[3];
                }
            }
        }
        // The X variants carry no alpha at all; whatever the compositor left
        // in that byte is not transparency, and an OCR pass that honoured it
        // would read black text on black.
        for p in dst.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Wayland plumbing
// ---------------------------------------------------------------------------

/// An anonymous shared-memory segment, mapped for the life of one grab.
struct Memfd {
    fd: OwnedFd,
    ptr: *mut libc::c_void,
    len: usize,
}

impl Memfd {
    fn new(len: usize) -> Result<Memfd, String> {
        let name = c"oracle-eyes-capture";
        // SAFETY: a NUL-terminated literal and a flag constant.
        let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: `raw` is a fresh fd this process owns.
        let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw) };
        // SAFETY: `fd` is a valid memfd and `len` fits an off_t.
        if unsafe { libc::ftruncate(raw, len as libc::off_t) } < 0 {
            return Err(format!("ftruncate: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: mapping a memfd we just sized; the result is checked.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                raw,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!("mmap: {}", std::io::Error::last_os_error()));
        }
        Ok(Memfd { fd, ptr, len })
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: the mapping is `len` bytes and lives as long as `self`. The
        // compositor may write it concurrently, but only before it sends
        // `ready`, and this is called after.
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }
}

impl AsFd for Memfd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Drop for Memfd {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what `new` mapped.
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

/// One output as the registry hands it over, filled in across several events.
#[derive(Debug)]
struct OutputState {
    proxy: Option<WlOutput>,
    /// Position from `wl_output.geometry`, which is already in logical
    /// coordinates but carries no size — hence the mode/scale division.
    geom_pos: Option<(i32, i32)>,
    mode: Option<(i32, i32)>,
    scale: i32,
    transform: Transform,
}

impl Default for OutputState {
    fn default() -> Self {
        OutputState {
            proxy: None,
            geom_pos: None,
            mode: None,
            scale: 1,
            transform: Transform::Normal,
        }
    }
}

/// Per-grab session bookkeeping, reset before every capture.
#[derive(Debug, Default)]
struct SessionState {
    /// The session `grab()` is currently waiting on. Events from any other
    /// (already-destroyed) session are stale and discarded on arrival.
    active: Option<ExtImageCopyCaptureSessionV1>,
    size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    done: bool,
    stopped: bool,
    ready: bool,
    failed: Option<FailureReason>,
    frame_transform: Option<Transform>,
}

#[derive(Default)]
struct State {
    outputs: Vec<OutputState>,
    session: SessionState,
}

pub struct Capturer {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    shm: WlShm,
    source_mgr: ExtOutputImageCaptureSourceManagerV1,
    capture_mgr: ExtImageCopyCaptureManagerV1,
}

impl Capturer {
    /// Connect and bind. An absent capture global is reported as the policy
    /// decision it is, with the line the owner has to add — "fail visibly"
    /// means the user learns what to do, not just that something broke.
    pub fn connect() -> Result<Capturer, String> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("cannot reach the Wayland display: {e}"))?;
        let (globals, mut queue): (GlobalList, EventQueue<State>) =
            registry_queue_init(&conn).map_err(|e| format!("wayland registry: {e}"))?;
        let qh = queue.handle();
        let mut state = State::default();

        let shm: WlShm = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("wl_shm is missing: {e}"))?;
        let source_mgr: ExtOutputImageCaptureSourceManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| denied("ext_output_image_capture_source_manager_v1"))?;
        let capture_mgr: ExtImageCopyCaptureManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| denied("ext_image_copy_capture_manager_v1"))?;

        // Outputs are enumerated once. Hotplug during the few milliseconds of
        // a grab is not handled: the worst case is one failed capture with a
        // visible error, and a resident registry listener would keep this
        // client awake for events it has no use for.
        for global in globals.contents().clone_list() {
            if global.interface != WlOutput::interface().name {
                continue;
            }
            let idx = state.outputs.len();
            let version = global.version.min(4);
            let output: WlOutput = globals.registry().bind(global.name, version, &qh, idx);
            state.outputs.push(OutputState {
                proxy: Some(output),
                ..OutputState::default()
            });
        }
        if state.outputs.is_empty() {
            return Err("the compositor advertises no wl_output".into());
        }

        let deadline = Instant::now() + SETUP_TIMEOUT;
        pump(
            &conn,
            &mut queue,
            &mut state,
            deadline,
            "output geometry",
            |s| s.outputs.iter().all(|o| o.mode.is_some()),
        )?;

        Ok(Capturer {
            conn,
            queue,
            qh,
            state,
            shm,
            source_mgr,
            capture_mgr,
        })
    }

    /// Resolved geometry for every known output, in registry order.
    fn geometry(&self) -> Vec<OutputGeom> {
        self.state
            .outputs
            .iter()
            .map(|o| {
                let (mw, mh) = o.mode.unwrap_or((0, 0));
                // The logical size is the mode divided by the integer scale:
                // exactly right for integer scaling, the best available guess
                // under fractional (see the module doc on xdg-output).
                let scale = o.scale.max(1);
                let (lw, lh) = (mw / scale, mh / scale);
                let (lx, ly) = o.geom_pos.unwrap_or((0, 0));
                OutputGeom {
                    logical: Region {
                        x: lx,
                        y: ly,
                        w: lw,
                        h: lh,
                    },
                    buf_w: mw,
                    buf_h: mh,
                }
            })
            .collect()
    }

    /// The logical rectangle of every known output, in registry order.
    /// Automatic mode (spec §2.2) needs somewhere to point when nobody has
    /// dragged a selection; this is that.
    pub fn output_regions(&self) -> Vec<Region> {
        self.geometry().into_iter().map(|g| g.logical).collect()
    }

    pub fn grab(&mut self, region: Region) -> Result<Frame, String> {
        if region.w <= 0 || region.h <= 0 {
            return Err(format!("empty capture region {region:?}"));
        }
        let geoms = self.geometry();
        let idx = pick_output(&geoms, region).ok_or_else(|| {
            format!(
                "no output contains ({}, {}); outputs are {}",
                region.x,
                region.y,
                describe(&geoms)
            )
        })?;
        let out = geoms[idx];
        if self.state.outputs[idx].transform != Transform::Normal {
            // A rotated output means the capture buffer is in a different
            // basis than the logical rectangle, and cropping it with this
            // maths would silently return the wrong part of the screen.
            return Err(format!(
                "output is rotated ({:?}); rotated capture is not implemented",
                self.state.outputs[idx].transform
            ));
        }
        let output = self.state.outputs[idx]
            .proxy
            .clone()
            .ok_or("output disappeared")?;

        self.state.session = SessionState::default();
        let deadline = Instant::now() + GRAB_TIMEOUT;

        let source = self.source_mgr.create_source(&output, &self.qh, ());
        // No `paint_cursors`: the pointer is not screen content, and leaving
        // it out of the buffer keeps it out of OCR.
        let session = self.capture_mgr.create_session(
            &source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            &self.qh,
            (),
        );
        self.state.session.active = Some(session.clone());
        let result = self.grab_inner(&session, out, region, deadline);

        // Tear down in every path: a live session costs the compositor a
        // capture pass per frame and keeps the capture indicator lit.
        session.destroy();
        source.destroy();
        let _ = self.conn.flush();
        result
    }

    fn grab_inner(
        &mut self,
        session: &ExtImageCopyCaptureSessionV1,
        out: OutputGeom,
        region: Region,
        deadline: Instant,
    ) -> Result<Frame, String> {
        pump(
            &self.conn,
            &mut self.queue,
            &mut self.state,
            deadline,
            "capture session constraints",
            |s| s.session.done || s.session.stopped,
        )?;
        if self.state.session.stopped {
            return Err(denied("a capture session"));
        }
        let (bw, bh) = self
            .state
            .session
            .size
            .ok_or("the compositor sent no buffer_size")?;
        let (bw, bh) = (bw as i32, bh as i32);
        // The advertised buffer is the whole output. If the mode we read from
        // wl_output disagrees, trust the session — it is what the buffer must
        // match — and crop against that.
        let out = OutputGeom {
            buf_w: bw,
            buf_h: bh,
            ..out
        };
        let fmt = self
            .state
            .session
            .formats
            .iter()
            .find_map(|f| PixFmt::from_shm(*f))
            .ok_or_else(|| {
                format!(
                    "no readable shm format offered (got {:?})",
                    self.state.session.formats
                )
            })?;
        let shm_fmt = *self
            .state
            .session
            .formats
            .iter()
            .find(|f| PixFmt::from_shm(**f) == Some(fmt))
            .expect("just matched");

        let crop = logical_to_buffer(region, &out)
            .ok_or_else(|| format!("{region:?} lies outside the captured output"))?;

        let stride = (bw as usize) * 4;
        let len = stride * (bh as usize);
        let mem = Memfd::new(len)?;
        let pool = self.shm.create_pool(mem.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, bw, bh, stride as i32, shm_fmt, &self.qh, ());

        let frame = session.create_frame(&self.qh, ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, bw, bh);
        frame.capture();

        let waited = pump(
            &self.conn,
            &mut self.queue,
            &mut self.state,
            deadline,
            "capture frame",
            |s| s.session.ready || s.session.failed.is_some() || s.session.stopped,
        );

        frame.destroy();
        buffer.destroy();
        pool.destroy();
        waited?;

        if let Some(reason) = self.state.session.failed {
            return Err(format!("the compositor refused the frame: {reason:?}"));
        }
        if !self.state.session.ready {
            return Err(denied("a capture frame"));
        }
        if let Some(t) = self.state.session.frame_transform {
            if t != Transform::Normal {
                return Err(format!("frame arrived with transform {t:?}"));
            }
        }

        let pixels = crop_convert(mem.as_slice(), stride, fmt, crop)?;
        Ok(Frame {
            width: crop.w as u32,
            height: crop.h as u32,
            stride: (crop.w as u32) * 4,
            pixels,
            origin: buffer_to_logical(crop, &out),
        })
    }
}

/// The one message the owner can act on.
fn denied(what: &str) -> String {
    format!(
        "capture capability denied: the compositor did not offer {what}. \
         Add `capture {{ allow \"oracle-eyes\" }}` to policy.kdl (it hot-reloads)."
    )
}

fn describe(geoms: &[OutputGeom]) -> String {
    geoms
        .iter()
        .map(|g| {
            format!(
                "{}x{}+{}+{}",
                g.logical.w, g.logical.h, g.logical.x, g.logical.y
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Dispatch until `done(&state)` or the deadline passes.
///
/// `EventQueue::roundtrip` would block forever on a compositor that stalls
/// mid-session, and a stalled daemon is invisible to the user — so every wait
/// in this module goes through here, where the fd is polled with the time
/// actually left and a timeout becomes an ordinary `Err`.
fn pump(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    deadline: Instant,
    what: &str,
    done: impl Fn(&State) -> bool,
) -> Result<(), String> {
    loop {
        queue
            .dispatch_pending(state)
            .map_err(|e| format!("wayland dispatch: {e}"))?;
        if done(state) {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        queue.flush().map_err(|e| format!("wayland flush: {e}"))?;
        // `prepare_read` returning None means another dispatch is owed first.
        let Some(guard) = conn.prepare_read() else {
            continue;
        };
        let ms = (deadline - now).as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&guard.connection_fd()),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialised pollfd, count 1.
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll: {err}"));
        }
        if n == 0 {
            return Err(format!("timed out waiting for {what}"));
        }
        if let Err(e) = guard.read() {
            return Err(format!("wayland read: {e}"));
        }
    }
}

// --- event handling --------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Hotplug is out of scope for a one-shot grab; see `connect`.
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        &idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.outputs.get_mut(idx) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                out.geom_pos = Some((x, y));
                if let WEnum::Value(t) = transform {
                    out.transform = t;
                }
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // Only the current mode describes the capture buffer; the
                // compositor may also list others.
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    out.mode = Some((width, height));
                }
            }
            wl_output::Event::Scale { factor } => out.scale = factor.max(1),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // A session already torn down by a prior `grab()` can still have
        // events in flight (e.g. a trailing `Stopped` sent as the compositor
        // tears it down). `state.session` is reset for the *next* session
        // before that arrives, so an unscoped match here would corrupt the
        // new grab's state with the old one's outcome. Only the session
        // `grab()` is currently waiting on may write into it.
        if state.session.active.as_ref() != Some(proxy) {
            return;
        }
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.session.size = Some((width, height))
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(f),
            } => state.session.formats.push(f),
            ext_image_copy_capture_session_v1::Event::Done => state.session.done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => state.session.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => state.session.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                state.session.failed = Some(match reason {
                    WEnum::Value(r) => r,
                    WEnum::Unknown(_) => FailureReason::Unknown,
                })
            }
            ext_image_copy_capture_frame_v1::Event::Transform {
                transform: WEnum::Value(t),
            } => state.session.frame_transform = Some(t),
            _ => {}
        }
    }
}

// Interfaces whose events say nothing this client acts on: `wl_shm`'s global
// format list is irrelevant (the session dictates the format), and the rest
// are pure factories or destructor-only objects.
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ExtImageCaptureSourceV1);
delegate_noop!(State: ignore ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ignore ExtImageCopyCaptureManagerV1);

#[cfg(test)]
mod tests {
    use super::*;

    fn out(x: i32, y: i32, w: i32, h: i32, bw: i32, bh: i32) -> OutputGeom {
        OutputGeom {
            logical: Region { x, y, w, h },
            buf_w: bw,
            buf_h: bh,
        }
    }

    #[test]
    fn xbgr_is_already_rgba_and_xrgb_is_swapped() {
        // One pixel whose bytes in memory are 01 02 03 04.
        let src = vec![1, 2, 3, 4];
        let r = PixRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        assert_eq!(
            crop_convert(&src, 4, PixFmt::Rgba, r).unwrap(),
            vec![1, 2, 3, 255]
        );
        assert_eq!(
            crop_convert(&src, 4, PixFmt::Bgra, r).unwrap(),
            vec![3, 2, 1, 255]
        );
    }

    #[test]
    fn shm_names_map_to_memory_order() {
        assert_eq!(
            PixFmt::from_shm(wl_shm::Format::Xbgr8888),
            Some(PixFmt::Rgba)
        );
        assert_eq!(
            PixFmt::from_shm(wl_shm::Format::Argb8888),
            Some(PixFmt::Bgra)
        );
        assert_eq!(PixFmt::from_shm(wl_shm::Format::Rgb565), None);
    }

    #[test]
    fn crop_reads_the_right_rows_and_ignores_padding() {
        // 3x2 image, stride padded to 16 bytes, RGBA already.
        let mut src = vec![0u8; 16 * 2];
        for (i, px) in [10u8, 20, 30].iter().enumerate() {
            src[16 + i * 4] = *px; // second row, red channel
        }
        let got = crop_convert(
            &src,
            16,
            PixFmt::Rgba,
            PixRect {
                x: 1,
                y: 1,
                w: 2,
                h: 1,
            },
        )
        .unwrap();
        assert_eq!(got, vec![20, 0, 0, 255, 30, 0, 0, 255]);
    }

    #[test]
    fn crop_past_the_end_is_an_error_not_a_panic() {
        let src = vec![0u8; 16];
        assert!(crop_convert(
            &src,
            16,
            PixFmt::Rgba,
            PixRect {
                x: 0,
                y: 0,
                w: 8,
                h: 2
            }
        )
        .is_err());
    }

    #[test]
    fn output_is_chosen_by_the_regions_top_left() {
        let outs = [
            out(0, 0, 1920, 1080, 1920, 1080),
            out(1920, 0, 2560, 1440, 2560, 1440),
        ];
        assert_eq!(
            pick_output(
                &outs,
                Region {
                    x: 10,
                    y: 10,
                    w: 4,
                    h: 4
                }
            ),
            Some(0)
        );
        assert_eq!(
            pick_output(
                &outs,
                Region {
                    x: 2000,
                    y: 10,
                    w: 4,
                    h: 4
                }
            ),
            Some(1)
        );
        // Straddling picks the output the drag started on.
        assert_eq!(
            pick_output(
                &outs,
                Region {
                    x: 1900,
                    y: 10,
                    w: 200,
                    h: 4
                }
            ),
            Some(0)
        );
        assert_eq!(
            pick_output(
                &outs,
                Region {
                    x: -1,
                    y: 0,
                    w: 4,
                    h: 4
                }
            ),
            None
        );
    }

    #[test]
    fn hidpi_region_scales_into_buffer_pixels() {
        let o = out(1920, 0, 1920, 1080, 3840, 2160);
        assert_eq!(
            logical_to_buffer(
                Region {
                    x: 1930,
                    y: 5,
                    w: 100,
                    h: 50
                },
                &o
            ),
            Some(PixRect {
                x: 20,
                y: 10,
                w: 200,
                h: 100
            })
        );
    }

    #[test]
    fn region_is_clamped_to_the_output() {
        let o = out(0, 0, 1920, 1080, 1920, 1080);
        let c = logical_to_buffer(
            Region {
                x: 1900,
                y: 1070,
                w: 200,
                h: 200,
            },
            &o,
        )
        .unwrap();
        assert_eq!(
            c,
            PixRect {
                x: 1900,
                y: 1070,
                w: 20,
                h: 10
            }
        );
        // And a region entirely off the output is None, not a zero-size crop.
        assert_eq!(
            logical_to_buffer(
                Region {
                    x: 5000,
                    y: 0,
                    w: 10,
                    h: 10
                },
                &o
            ),
            None
        );
    }

    #[test]
    fn origin_round_trips_back_to_logical() {
        let o = out(1920, 0, 1920, 1080, 3840, 2160);
        let r = Region {
            x: 1930,
            y: 5,
            w: 100,
            h: 50,
        };
        let c = logical_to_buffer(r, &o).unwrap();
        assert_eq!(buffer_to_logical(c, &o), r);
    }

    #[test]
    fn fractional_scale_uses_the_ratio_not_the_integer_scale() {
        // 1.5x: 2560x1440 panel presented as 1707x960 logical.
        let o = out(0, 0, 1707, 960, 2560, 1440);
        let c = logical_to_buffer(
            Region {
                x: 100,
                y: 100,
                w: 100,
                h: 100,
            },
            &o,
        )
        .unwrap();
        // Outward rounding, so at least the requested area is covered.
        assert!(c.w >= 150 && c.h >= 150, "{c:?}");
        assert!(c.x <= 150 && c.y <= 150, "{c:?}");
    }
}
