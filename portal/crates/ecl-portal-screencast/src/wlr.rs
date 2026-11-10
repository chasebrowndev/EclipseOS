// SPDX-License-Identifier: AGPL-3.0-only

//! `zwlr_screencopy_v1` client (wl_shm only) against abyss: outputs only.
//! Windows and virtual sources need a toplevel image-capture source in abyss
//! (ADR 0097) and are refused here. Cursor: `Embedded` maps to
//! `overlay_cursor = 1`; abyss refuses cursor sessions, so `Metadata` is
//! never requested (the session layer does not offer it).
//!
//! Capture authority is entirely abyss's (ADR 0027/0097): if abyss does not
//! admit this process, the manager global is absent or frames `failed`, and
//! we surface that as an error. No retry loops that could mask a denial.

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use ecl_portal_proto::Source;
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{memfd_create, MemfdFlags};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_output::{self, WlOutput},
    wl_registry::WlRegistry,
    wl_shm::{self, WlShm},
    wl_shm_pool::{self, WlShmPool},
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};

use crate::session::CursorMode;
use crate::source::{CaptureError, CaptureSource, Capturer, Frame, PixelFormat, Rect};

fn io<E: std::fmt::Display>(e: E) -> CaptureError {
    CaptureError::Io(e.to_string())
}

#[derive(Default)]
struct Pending {
    shm_fmt: Option<(u32, u32, u32, u32)>, // wl format, w, h, stride
    buffer_done: bool,
    copied: bool,
    y_invert: bool,
    damage: Vec<Rect>,
    ready_ns: Option<u64>,
    failed: bool,
}

struct ShmBuf {
    file: File,
    buffer: WlBuffer,
    w: u32,
    h: u32,
    stride: u32,
    fmt: u32,
}

struct State {
    outputs: Vec<(WlOutput, Option<String>)>,
    frame: Option<(ZwlrScreencopyFrameV1, Pending)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        s: &mut Self,
        o: &WlOutput,
        e: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = e {
            if let Some(slot) = s.outputs.iter_mut().find(|(p, _)| p == o) {
                slot.1 = Some(name);
            }
        }
    }
}

macro_rules! ignore {
    ($($proxy:ty => $ev:ty),*) => {$(
        impl Dispatch<$proxy, ()> for State {
            fn event(_: &mut Self, _: &$proxy, _: $ev, _: &(), _: &Connection,
                _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlShm => wl_shm::Event, WlShmPool => wl_shm_pool::Event, WlBuffer => wl_buffer::Event,
    ZwlrScreencopyManagerV1 => zwlr_screencopy_manager_v1::Event);

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        s: &mut Self,
        f: &ZwlrScreencopyFrameV1,
        e: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event as E;
        let Some((cur, p)) = s.frame.as_mut() else {
            return;
        };
        if cur != f {
            return;
        }
        match e {
            E::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let code = match format {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(u) => u,
                };
                p.shm_fmt.get_or_insert((code, width, height, stride));
            }
            E::BufferDone => p.buffer_done = true,
            E::Flags { flags } => {
                p.y_invert = matches!(flags, WEnum::Value(v) if v.contains(zwlr_screencopy_frame_v1::Flags::YInvert));
            }
            E::Damage {
                x,
                y,
                width,
                height,
            } => p.damage.push(Rect {
                x,
                y,
                w: width,
                h: height,
            }),
            E::Ready {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                let secs = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                p.ready_ns = Some(secs.saturating_mul(1_000_000_000) + u64::from(tv_nsec));
            }
            E::Failed => p.failed = true,
            _ => {}
        }
    }
}

pub struct WlrSource {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    shm: WlShm,
    mgr: ZwlrScreencopyManagerV1,
    output: WlOutput,
    overlay_cursor: bool,
    buf: Option<ShmBuf>,
    first: bool,
}

impl WlrSource {
    /// Connect to `$WAYLAND_DISPLAY` and bind the output called `output_name`
    /// (empty = first).
    pub fn connect(output_name: &str, cursor: CursorMode) -> Result<Self, CaptureError> {
        let conn = Connection::connect_to_env().map_err(io)?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(io)?;
        let qh = queue.handle();
        let mgr: ZwlrScreencopyManagerV1 = globals
            .bind(&qh, 1..=3, ())
            .map_err(|_| CaptureError::Denied)?; // not visible = not allowlisted (ADR 0027)
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(io)?;
        let mut state = State {
            outputs: Vec::new(),
            frame: None,
        };
        let names: Vec<_> = globals.contents().with_list(|l| {
            l.iter()
                .filter(|g| g.interface == "wl_output")
                .map(|g| (g.name, g.version))
                .collect()
        });
        for (name, ver) in names {
            let o: WlOutput = globals.registry().bind(name, ver.min(4), &qh, ());
            state.outputs.push((o, None));
        }
        queue.roundtrip(&mut state).map_err(io)?;
        let output = state
            .outputs
            .iter()
            .find(|(_, n)| output_name.is_empty() || n.as_deref() == Some(output_name))
            .map(|(o, _)| o.clone())
            .ok_or(CaptureError::Unsupported("no such output"))?;
        Ok(Self {
            conn,
            queue,
            qh,
            state,
            shm,
            mgr,
            output,
            overlay_cursor: cursor == CursorMode::Embedded,
            buf: None,
            first: true,
        })
    }

    fn request(&mut self) {
        let f = self
            .mgr
            .capture_output(i32::from(self.overlay_cursor), &self.output, &self.qh, ());
        self.state.frame = Some((f, Pending::default()));
    }

    fn ensure_buf(&mut self, fmt: u32, w: u32, h: u32, stride: u32) -> Result<(), CaptureError> {
        if let Some(b) = &self.buf {
            if (b.fmt, b.w, b.h, b.stride) == (fmt, w, h, stride) {
                return Ok(());
            }
        }
        if let Some(old) = self.buf.take() {
            old.buffer.destroy();
        }
        let wlfmt = wl_shm::Format::try_from(fmt).map_err(|_| CaptureError::Format(fmt))?;
        let len = u64::from(stride) * u64::from(h);
        if len == 0 || len > (1 << 30) {
            return Err(CaptureError::Failed);
        }
        let file = File::from(memfd_create("ecl-screencast", MemfdFlags::CLOEXEC).map_err(io)?);
        file.set_len(len).map_err(io)?;
        let pool = self.shm.create_pool(file.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wlfmt, &self.qh, ());
        pool.destroy();
        self.buf = Some(ShmBuf {
            file,
            buffer,
            w,
            h,
            stride,
            fmt,
        });
        Ok(())
    }

    /// Send `copy` once the buffer description is complete.
    fn advance(&mut self) -> Result<(), CaptureError> {
        let ready = match &self.state.frame {
            Some((_, p)) if !p.copied && p.shm_fmt.is_some() => {
                p.buffer_done || self.mgr.version() < 3
            }
            _ => false,
        };
        if !ready {
            return Ok(());
        }
        let (fmt, w, h, stride) = self
            .state
            .frame
            .as_ref()
            .and_then(|(_, p)| p.shm_fmt)
            .expect("checked");
        self.ensure_buf(fmt, w, h, stride)?;
        let buffer = &self.buf.as_ref().expect("ensured").buffer;
        let (f, p) = self.state.frame.as_mut().expect("checked");
        // The first frame must be unconditional; later ones wait for damage.
        if self.first || f.version() < 2 {
            f.copy(buffer);
        } else {
            f.copy_with_damage(buffer);
        }
        p.copied = true;
        Ok(())
    }

    fn pump(&mut self, timeout: Duration) -> Result<(), CaptureError> {
        self.queue.dispatch_pending(&mut self.state).map_err(io)?;
        self.conn.flush().map_err(io)?;
        if let Some(guard) = self.queue.prepare_read() {
            let ts = Timespec {
                tv_sec: timeout.as_secs() as _,
                tv_nsec: timeout.subsec_nanos() as _,
            };
            let readable = {
                let fd = guard.connection_fd();
                let mut fds = [PollFd::new(&fd, PollFlags::IN)];
                poll(&mut fds, Some(&ts)).map_err(io)? > 0
            };
            if readable {
                guard.read().map_err(io)?;
            } else {
                drop(guard);
            }
        }
        self.queue.dispatch_pending(&mut self.state).map_err(io)?;
        Ok(())
    }
}

impl CaptureSource for WlrSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        if self.state.frame.is_none() {
            self.request();
        }
        loop {
            self.advance()?;
            let (failed, done) = match &self.state.frame {
                Some((_, p)) => (p.failed, p.ready_ns.is_some()),
                None => unreachable!("request() above"),
            };
            if failed {
                if let Some((f, _)) = self.state.frame.take() {
                    f.destroy();
                }
                return Err(CaptureError::Failed);
            }
            if done {
                let (f, p) = self.state.frame.take().expect("some");
                f.destroy();
                self.first = false;
                let b = self.buf.as_ref().ok_or(CaptureError::Failed)?;
                let format = PixelFormat::from_wl(b.fmt).ok_or(CaptureError::Format(b.fmt))?;
                let mut data = vec![0u8; b.stride as usize * b.h as usize];
                b.file.read_exact_at(&mut data, 0).map_err(io)?;
                if p.y_invert {
                    let s = b.stride as usize;
                    let rows = b.h as usize;
                    for y in 0..rows / 2 {
                        let (a, z) = data.split_at_mut((rows - 1 - y) * s);
                        a[y * s..(y + 1) * s].swap_with_slice(&mut z[..s]);
                    }
                }
                return Ok(Some(Frame {
                    width: b.w,
                    height: b.h,
                    stride: b.stride,
                    format,
                    data,
                    damage: p.damage,
                    pts_ns: p.ready_ns.unwrap_or(0),
                }));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None); // frame stays in flight for the next call
            }
            self.pump(left)?;
        }
    }
}

/// Opens one screencopy connection per source (each stream owns its socket).
pub struct WlrCapturer;

impl Capturer for WlrCapturer {
    fn open(
        &self,
        source: &Source,
        cursor: CursorMode,
    ) -> Result<Box<dyn CaptureSource>, CaptureError> {
        match source {
            Source::Output { name, .. } => Ok(Box::new(WlrSource::connect(name, cursor)?)),
            Source::Window { .. } => Err(CaptureError::Unsupported(
                "window capture needs a toplevel image-capture source in abyss",
            )),
        }
    }
}

// `WlrSource` holds wayland proxies, which are Send + Sync in wayland-client.
