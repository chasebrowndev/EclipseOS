// SPDX-License-Identifier: AGPL-3.0-only
//! Built-in screenshots (ADR 0093).
//!
//! Both actions render through [`capture::capture_elements`], the one place
//! redaction is decided (ADR 0030), so a shot has the screen's blur, glass,
//! rounding and shadow and can never show a surface a capture would withhold.
//! The selector the region action drives is the untrusted compositor pass in
//! `ec-abyss-render::select`; it is drawn by the backends, so it is not in the
//! frame this module renders.
//!
//! The region action renders when its bind is pressed and keeps that frame, so
//! the shot is the moment of the keypress however long the drag takes. PNG
//! encoding and the file write happen on a thread and return by channel; the
//! compositor then offers the PNG as the clipboard selection. Clipboard
//! contents are never logged.

use std::{io::Write, os::fd::OwnedFd, path::PathBuf, sync::Arc};

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        gles::{GlesRenderer, GlesTexture},
        Bind, ExportMem, Offscreen,
    },
    output::Output,
    reexports::calloop::channel::{self, Event},
    utils::{Buffer as BufferCoord, Logical, Point, Rectangle, Size},
};

use crate::{
    render::capture::{self, FORMAT},
    state::AbyssState,
};

/// Opaque black, as the capture pass clears to.
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const MIME: &str = "image/png";

enum Request {
    Full,
    /// Render the frame the region selection will crop.
    Freeze,
    Crop(Rectangle<i32, Logical>),
}

/// A frame held for a region selection in progress.
struct Frozen {
    texture: GlesTexture,
    geo: Rectangle<i32, Logical>,
    scale: f64,
    size: Size<i32, BufferCoord>,
    target: String,
    redacted: Vec<[i32; 4]>,
}

#[derive(Default)]
pub struct Shot {
    request: Option<Request>,
    frozen: Option<Frozen>,
    /// The last shot, offered as the clipboard selection until someone else
    /// sets one.
    clip: Option<Arc<Vec<u8>>>,
}

/// What the writer thread hands back.
struct Saved {
    png: Arc<Vec<u8>>,
    path: Option<PathBuf>,
    hash: [u8; 32],
    kind: &'static str,
    target: String,
    region: Rectangle<i32, Logical>,
    scale_milli: u64,
    redacted: Vec<[i32; 4]>,
}

impl Shot {
    pub fn full(&mut self) {
        self.request = Some(Request::Full);
    }

    /// The region bind: the next frame is frozen and the selector crops it.
    pub fn begin_region(&mut self) {
        self.frozen = None;
        self.request = Some(Request::Freeze);
    }

    /// Whether the region selector is serving a screenshot rather than an
    /// add-on.
    pub fn selecting(&self) -> bool {
        self.frozen.is_some() || matches!(self.request, Some(Request::Freeze))
    }

    /// The selector committed `rect` (global logical).
    pub fn crop(&mut self, rect: Rectangle<i32, Logical>) {
        if self.frozen.is_some() {
            self.request = Some(Request::Crop(rect));
        }
    }

    /// The last shot's PNG, if its selection is still the clipboard.
    pub fn clipboard(&self, mime: &str) -> Option<Arc<Vec<u8>>> {
        self.clip.clone().filter(|_| mime == MIME)
    }

    /// Someone else set the selection.
    pub fn forget_clipboard(&mut self) {
        self.clip = None;
    }
}

/// Write the PNG into a reader's pipe off the loop: a reader that never
/// drains it must not stall the compositor.
pub fn serve_clipboard(png: Arc<Vec<u8>>, fd: OwnedFd) {
    std::thread::spawn(move || {
        let _ = std::fs::File::from(fd).write_all(&png);
    });
}

/// Run what the bind queued. Called once per frame from [`capture::service`],
/// which owns the renderer.
pub fn service(state: &mut AbyssState, renderer: &mut GlesRenderer) {
    match state.shot.request.take() {
        Some(Request::Full) => match under_pointer(state) {
            Some(output) => match render(state, renderer, &output) {
                Ok(mut frame) => {
                    let all = Rectangle::from_size(frame.size);
                    save(state, renderer, &mut frame, all, "screenshot");
                }
                Err(e) => tracing::warn!(error = %e, "screenshot failed"),
            },
            None => tracing::warn!("screenshot: no output under the pointer"),
        },
        Some(Request::Freeze) => {
            let frame = under_pointer(state).map(|o| render(state, renderer, &o));
            match frame {
                Some(Ok(frame)) => state.shot.frozen = Some(frame),
                other => {
                    if let Some(Err(e)) = other {
                        tracing::warn!(error = %e, "screenshot failed");
                    }
                    state.region_select.cancel();
                    crate::backend::damage_all(state);
                }
            }
        }
        Some(Request::Crop(rect)) => {
            if let Some(mut frame) = state.shot.frozen.take() {
                if let Some(px) = crop_px(frame.geo, frame.scale, frame.size, rect) {
                    save(state, renderer, &mut frame, px, "screenshot-region");
                }
            }
        }
        None => {}
    }
    // A held frame lives as long as the selection that wants it: Esc, the
    // chord again, a lock or a hook change all end the selection.
    if state.shot.request.is_none() && !state.region_select.active() {
        state.shot.frozen = None;
    }
}

fn under_pointer(state: &AbyssState) -> Option<Output> {
    let at = state.pointer_location;
    state
        .space
        .outputs()
        .find(|o| {
            state
                .space
                .output_geometry(o)
                .is_some_and(|g| g.to_f64().contains(at))
        })
        .or_else(|| state.space.outputs().next())
        .cloned()
}

/// `rect` (global logical) as pixels of a frame of `size` that shows `geo`
/// at `scale`, or `None` when it misses the frame.
fn crop_px(
    geo: Rectangle<i32, Logical>,
    scale: f64,
    size: Size<i32, BufferCoord>,
    rect: Rectangle<i32, Logical>,
) -> Option<Rectangle<i32, BufferCoord>> {
    let local = rect.intersection(geo)?;
    let at = |v: i32, origin: i32, max: i32, up: bool| {
        let px = f64::from(v - origin) * scale;
        (if up { px.ceil() } else { px.floor() } as i32).clamp(0, max)
    };
    let x0 = at(local.loc.x, geo.loc.x, size.w, false);
    let y0 = at(local.loc.y, geo.loc.y, size.h, false);
    let x1 = at(local.loc.x + local.size.w, geo.loc.x, size.w, true);
    let y1 = at(local.loc.y + local.size.h, geo.loc.y, size.h, true);
    (x1 > x0 && y1 > y0).then(|| Rectangle::new(Point::from((x0, y0)), Size::from((x1 - x0, y1 - y0))))
}

/// The capture pass list for `output`, composited into a texture. The cursor
/// is left out, as in a still from any screenshot tool.
fn render(state: &mut AbyssState, renderer: &mut GlesRenderer, output: &Output) -> anyhow::Result<Frozen> {
    let mode = output
        .current_mode()
        .ok_or_else(|| anyhow::anyhow!("output has no mode"))?;
    let geo = state
        .space
        .output_geometry(output)
        .ok_or_else(|| anyhow::anyhow!("output is not in the space"))?;
    let mut still = std::mem::take(&mut state.capture_fx);
    let (elements, redacted) = capture::capture_elements(renderer, state, &mut still, output);
    state.capture_fx = still;
    let size = output.current_transform().transform_size(mode.size);
    let size: Size<i32, BufferCoord> = Size::from((size.w, size.h));
    let mut texture: GlesTexture = Offscreen::create_buffer(renderer, FORMAT, size)?;
    {
        let mut fb = Bind::bind(renderer, &mut texture)?;
        OutputDamageTracker::from_output(output)
            .render_output(renderer, &mut fb, 0, &elements, CLEAR)
            .map_err(|e| anyhow::anyhow!("render screenshot: {e:?}"))?;
    }
    Ok(Frozen {
        texture,
        geo,
        scale: output.current_scale().fractional_scale(),
        size,
        target: output.name(),
        redacted: redacted
            .iter()
            .flat_map(|r| r.rects.iter())
            .map(|r| [r.loc.x, r.loc.y, r.size.w, r.size.h])
            .collect(),
    })
}

/// Read `px` back and hand it to the writer thread.
fn save(
    state: &mut AbyssState,
    renderer: &mut GlesRenderer,
    frame: &mut Frozen,
    px: Rectangle<i32, BufferCoord>,
    kind: &'static str,
) {
    let pixels = (|| -> anyhow::Result<Vec<u8>> {
        let fb = Bind::bind(renderer, &mut frame.texture)?;
        let mapping = ExportMem::copy_framebuffer(renderer, &fb, px, FORMAT)?;
        Ok(ExportMem::map_texture(renderer, &mapping)?.to_vec())
    })();
    let pixels = match pixels {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "screenshot readback failed");
            return;
        }
    };
    // Logical box of what was read, for the audit record.
    let s = frame.scale;
    let region = Rectangle::new(
        frame.geo.loc + Point::from(((px.loc.x as f64 / s) as i32, (px.loc.y as f64 / s) as i32)),
        Size::from(((px.size.w as f64 / s) as i32, (px.size.h as f64 / s) as i32)),
    );
    let (w, h) = (px.size.w as u32, px.size.h as u32);
    let (target, redacted) = (frame.target.clone(), frame.redacted.clone());
    let scale_milli = (s * 1000.0).round() as u64;

    let (tx, rx) = channel::channel::<Saved>();
    let inserted = state.loop_handle.insert_source(rx, |event, _, state| {
        if let Event::Msg(saved) = event {
            finish(state, saved);
        }
    });
    if let Err(e) = inserted {
        tracing::warn!(error = %e, "screenshot: could not queue the writer");
        return;
    }
    std::thread::spawn(move || {
        let png = match encode(&pixels, w, h) {
            Ok(png) => png,
            Err(e) => {
                tracing::warn!(error = %e, "screenshot: PNG encode failed");
                return;
            }
        };
        let hash = *blake3::hash(&png).as_bytes();
        let path = match write(&png) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "screenshot: could not write the file");
                None
            }
        };
        let _ = tx.send(Saved {
            png: Arc::new(png),
            path,
            hash,
            kind,
            target,
            region,
            scale_milli,
            redacted,
        });
    });
}

/// `Xbgr8888` bytes are R, G, B, X in memory; the X byte is undefined, so the
/// PNG is plain RGB.
fn encode(rgbx: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in rgbx.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    enc.write_header()?.write_image_data(&rgb)?;
    Ok(out)
}

/// `~/Pictures/Screenshots/<local time>.png`, never over an earlier shot.
fn write(png: &[u8]) -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let dir = PathBuf::from(home).join("Pictures").join("Screenshots");
    std::fs::create_dir_all(&dir)?;
    let stamp = stamp();
    for n in 0.. {
        let name = if n == 0 {
            format!("{stamp}.png")
        } else {
            format!("{stamp}-{n}.png")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(png)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

fn stamp() -> String {
    // SAFETY: `localtime_r` writes only into the zeroed `tm` it is given.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Back on the loop: take the clipboard and say what happened.
fn finish(state: &mut AbyssState, saved: Saved) {
    state.shot.clip = Some(saved.png);
    let seat = state.seat.clone();
    smithay::wayland::selection::data_device::set_data_device_selection(
        &state.display_handle,
        &seat,
        vec![MIME.to_string()],
        (),
    );
    match &saved.path {
        Some(p) => tracing::info!(path = %p.display(), "screenshot saved"),
        None => tracing::info!("screenshot on the clipboard only"),
    }
    let r = saved.region;
    crate::audit::capture(
        state,
        saved.kind,
        &saved.target,
        [r.loc.x, r.loc.y, r.size.w, r.size.h],
        saved.scale_milli,
        saved.hash,
        &saved.redacted,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn crop_is_in_frame_pixels_and_rounds_outward() {
        let size = Size::from((3840, 2160));
        // Second output at x=1920, scale 2: a rect 10 logical px in is 20 px in.
        let px = crop_px(r(1920, 0, 1920, 1080), 2.0, size, r(1930, 5, 100, 50)).unwrap();
        assert_eq!((px.loc.x, px.loc.y, px.size.w, px.size.h), (20, 10, 200, 100));
        // A fractional scale never loses an edge pixel.
        let px = crop_px(r(0, 0, 1280, 720), 1.5, Size::from((1920, 1080)), r(1, 1, 3, 3)).unwrap();
        assert_eq!((px.loc.x, px.size.w), (1, 5));
    }

    #[test]
    fn a_rect_off_the_frame_is_nothing_and_a_straddling_one_is_cut() {
        let size = Size::from((1920, 1080));
        assert!(crop_px(r(0, 0, 1920, 1080), 1.0, size, r(2000, 0, 50, 50)).is_none());
        let px = crop_px(r(0, 0, 1920, 1080), 1.0, size, r(1900, 1070, 100, 100)).unwrap();
        assert_eq!((px.size.w, px.size.h), (20, 10));
    }

    #[test]
    fn the_png_drops_the_undefined_x_byte() {
        let png = encode(&[1, 2, 3, 0, 4, 5, 6, 255], 2, 1).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_crop_needs_a_frozen_frame_and_the_clipboard_is_png_only() {
        let mut shot = Shot::default();
        shot.crop(r(0, 0, 10, 10));
        assert!(shot.request.is_none(), "nothing frozen, nothing to crop");
        assert!(!shot.selecting());
        shot.begin_region();
        assert!(shot.selecting());
        shot.clip = Some(Arc::new(vec![1]));
        assert!(shot.clipboard("image/png").is_some());
        assert!(shot.clipboard("text/plain").is_none());
        shot.forget_clipboard();
        assert!(shot.clipboard("image/png").is_none());
    }
}
