// SPDX-License-Identifier: AGPL-3.0-only
//! Offline preview of an add-on transition style (ADR 0073). Dev tool, not
//! shipped.
//!
//! ```text
//! cargo run --release -p ec-abyss-bench --bin shader-preview -- \
//!     <style> [--event open|close|minimize] [--travel X,Y] [--size WxH]
//!             [--frames N] [--out DIR] [--catalog DIR]
//! ```
//!
//! It opens a surfaceless EGL display on a render node (no display server, no
//! window, never the live session), loads the style through the real
//! `ec_abyss_config::transitions::load_from`, and plays it through the real
//! `Registry::start` / `Registry::element` path: the same `HEADER`, uniforms,
//! armed clock and offscreen copy the compositor uses. Frames are composited
//! over a dark backdrop and written as a contact sheet PNG and an animated
//! GIF; the average GPU time per frame at a 2560x1600 window is printed.

#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::context::EGLContext;
use smithay::backend::egl::native::EGLSurfacelessDisplay;
use smithay::backend::egl::{EGLDevice, EGLDisplay};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{
    Bind, Color32F, ExportMem, Frame, ImportMem, Offscreen, Renderer, TextureMapping,
};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};

use ec_abyss_config::animations::{Animations, Event, Override, Preset};
use ec_abyss_config::transitions;
use ec_abyss_render::anim::shed::ShedLevel;
use ec_abyss_render::anim::{RunKind, ShaderRegistry};

const DEFAULT_OUT: &str = "/tmp/claude-1000/-home-chase-syncedprojects-EclipseOS/af992ec3-fecc-4812-b45c-b442ccdf09b1/scratchpad/preview";
const DEFAULT_CATALOG: &str = "packaging/transitions/anim-pack";
const TIMING_SIZE: (i32, i32) = (2560, 1600);

struct Args {
    style: String,
    kind: RunKind,
    travel: Option<(f64, f64)>,
    size: (i32, i32),
    frames: usize,
    out: PathBuf,
    catalog: PathBuf,
}

fn usage() -> ! {
    eprintln!(
        "usage: shader-preview <style> [--event open|close|minimize] [--travel X,Y] [--size WxH]\n\
         \x20                     [--frames N] [--out DIR] [--catalog DIR]\n\
         <style> is `pack:style` or a bare style name; --catalog is a transitions root or one pack\n\
         directory (default {DEFAULT_CATALOG})."
    );
    std::process::exit(2)
}

fn pair<T: std::str::FromStr>(s: &str, sep: char) -> Option<(T, T)> {
    let (a, b) = s.split_once(sep)?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        style: String::new(),
        kind: RunKind::Close,
        travel: None,
        size: (1280, 800),
        frames: 24,
        out: PathBuf::from(DEFAULT_OUT),
        catalog: PathBuf::from(DEFAULT_CATALOG),
    };
    while let Some(arg) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--event" => {
                a.kind = match val().as_str() {
                    "open" => RunKind::Open,
                    "close" => RunKind::Close,
                    "minimize" => RunKind::Minimize,
                    _ => usage(),
                }
            }
            "--travel" => a.travel = Some(pair(&val(), ',').unwrap_or_else(|| usage())),
            "--size" => a.size = pair(&val(), 'x').unwrap_or_else(|| usage()),
            "--frames" => a.frames = val().parse().unwrap_or_else(|_| usage()),
            "--out" => a.out = PathBuf::from(val()),
            "--catalog" => a.catalog = PathBuf::from(val()),
            "-h" | "--help" => usage(),
            s if !s.starts_with('-') && a.style.is_empty() => a.style = s.to_owned(),
            _ => usage(),
        }
    }
    if a.style.is_empty() || a.frames < 2 || a.size.0 < 16 || a.size.1 < 16 {
        usage();
    }
    a
}

// ---------------------------------------------------------------- EGL

fn renderer() -> Result<GlesRenderer, String> {
    // A render node first (the real GPU); the Mesa surfaceless platform if
    // device enumeration is unavailable.
    let display = unsafe {
        match EGLDevice::enumerate() {
            Ok(devices) => {
                let mut all: Vec<EGLDevice> = devices.collect();
                all.sort_by_key(|d| d.is_software());
                match all.into_iter().next() {
                    Some(d) => {
                        eprintln!(
                            "egl device: {}",
                            d.render_device_path()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|_| "software".into())
                        );
                        EGLDisplay::new(d)
                    }
                    None => EGLDisplay::new(EGLSurfacelessDisplay),
                }
            }
            Err(_) => EGLDisplay::new(EGLSurfacelessDisplay),
        }
    }
    .map_err(|e| format!("EGL display: {e}"))?;
    let context = EGLContext::new(&display).map_err(|e| format!("EGL context: {e}"))?;
    unsafe { GlesRenderer::new(context) }.map_err(|e| format!("GLES renderer: {e}"))
}

// ---------------------------------------------------------------- images

/// Straight-alpha RGBA, row 0 at the top.
struct Image {
    w: i32,
    h: i32,
    px: Vec<u8>,
}

impl Image {
    fn new(w: i32, h: i32) -> Self {
        Image {
            w,
            h,
            px: vec![0; (w * h * 4) as usize],
        }
    }

    /// Source-over a rounded rectangle, anti-aliased by distance.
    fn rrect(&mut self, x: f32, y: f32, rw: f32, rh: f32, r: f32, c: [u8; 4]) {
        let (x0, y0) = (
            (x - 1.0).floor().max(0.0) as i32,
            (y - 1.0).floor().max(0.0) as i32,
        );
        let (x1, y1) = (
            ((x + rw + 1.0).ceil() as i32).min(self.w),
            ((y + rh + 1.0).ceil() as i32).min(self.h),
        );
        let (cx, cy) = (x + rw / 2.0, y + rh / 2.0);
        let r = r.min(rw / 2.0).min(rh / 2.0);
        for py in y0..y1 {
            for px in x0..x1 {
                let (dx, dy) = (
                    (px as f32 + 0.5 - cx).abs() - (rw / 2.0 - r),
                    (py as f32 + 0.5 - cy).abs() - (rh / 2.0 - r),
                );
                let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r;
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let a = cov * c[3] as f32 / 255.0;
                let i = ((py * self.w + px) * 4) as usize;
                let da = self.px[i + 3] as f32 / 255.0;
                let oa = a + da * (1.0 - a);
                for k in 0..3 {
                    let s = c[k] as f32 * a + self.px[i + k] as f32 * da * (1.0 - a);
                    self.px[i + k] = if oa > 0.0 { (s / oa).round() as u8 } else { 0 };
                }
                self.px[i + 3] = (oa * 255.0).round() as u8;
            }
        }
    }

    /// Premultiplied copy for GL.
    fn premultiplied(&self) -> Vec<u8> {
        let mut out = self.px.clone();
        for p in out.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
        out
    }
}

/// A dark glass panel: title bar, sidebar, text-like bars, a gold accent.
fn sample_window(w: i32, h: i32) -> Image {
    let mut im = Image::new(w, h);
    let k = (w.min(h * 8 / 5) as f32 / 1280.0).max(0.25);
    let (wf, hf) = (w as f32, h as f32);
    let radius = 14.0 * k;
    let gold = [212, 175, 55, 255];
    let ink = [226, 230, 240, 235];
    let dim = [122, 128, 148, 200];
    // Rim light, then the glass.
    im.rrect(0.0, 0.0, wf, hf, radius, [255, 255, 255, 46]);
    im.rrect(k, k, wf - 2.0 * k, hf - 2.0 * k, radius - k, [19, 21, 29, 238]);
    // Title bar and its dots.
    let bar = 46.0 * k;
    im.rrect(k, k, wf - 2.0 * k, bar, radius - k, [32, 35, 47, 255]);
    im.rrect(k, bar - 6.0 * k, wf - 2.0 * k, 8.0 * k, 0.0, [32, 35, 47, 255]);
    for (i, c) in [[255, 95, 87, 255], [254, 188, 46, 255], [40, 200, 64, 255]]
        .iter()
        .enumerate()
    {
        im.rrect(
            (18.0 + i as f32 * 22.0) * k,
            bar / 2.0 - 6.0 * k,
            12.0 * k,
            12.0 * k,
            6.0 * k,
            *c,
        );
    }
    im.rrect(
        wf / 2.0 - 90.0 * k,
        bar / 2.0 - 5.0 * k,
        180.0 * k,
        10.0 * k,
        5.0 * k,
        dim,
    );
    im.rrect(k, bar, wf * 0.2, 3.0 * k, 0.0, gold);
    // Sidebar.
    let side = wf * 0.22;
    im.rrect(
        k,
        bar + 3.0 * k,
        side,
        hf - bar - 3.0 * k - k,
        radius - k,
        [14, 16, 23, 255],
    );
    for i in 0..9 {
        let y = bar + (28.0 + i as f32 * 38.0) * k;
        let lw = side * (0.45 + 0.4 * ((i * 37 % 11) as f32 / 11.0));
        im.rrect(
            20.0 * k,
            y,
            lw,
            12.0 * k,
            6.0 * k,
            if i == 2 { gold } else { dim },
        );
    }
    // Content: a heading, paragraphs of bars, two cards.
    let cx = side + 40.0 * k;
    let cw = wf - cx - 40.0 * k;
    im.rrect(cx, bar + 36.0 * k, cw * 0.38, 26.0 * k, 8.0 * k, ink);
    im.rrect(cx, bar + 76.0 * k, cw * 0.18, 8.0 * k, 4.0 * k, gold);
    for i in 0..7 {
        let y = bar + (112.0 + i as f32 * 26.0) * k;
        let lw = cw * (0.55 + 0.4 * (((i * 5 + 3) % 7) as f32 / 7.0));
        im.rrect(cx, y, lw, 11.0 * k, 5.5 * k, dim);
    }
    let cy = bar + 316.0 * k;
    let ch = (hf - cy - 40.0 * k).max(20.0);
    let half = (cw - 24.0 * k) / 2.0;
    for j in 0..2 {
        let x = cx + j as f32 * (half + 24.0 * k);
        im.rrect(x, cy, half, ch, 14.0 * k, [255, 255, 255, 20]);
        im.rrect(x + 18.0 * k, cy + 18.0 * k, half * 0.4, 12.0 * k, 6.0 * k, ink);
        im.rrect(x + 18.0 * k, cy + 44.0 * k, half * 0.7, 9.0 * k, 4.5 * k, dim);
        if j == 1 {
            im.rrect(
                x + 18.0 * k,
                cy + ch - 46.0 * k,
                110.0 * k,
                28.0 * k,
                14.0 * k,
                gold,
            );
        }
    }
    im
}

/// A dark, wallpaper-like backdrop.
fn backdrop(w: i32, h: i32) -> Image {
    let mut im = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let t = (u * 0.6 + v * 0.4).clamp(0.0, 1.0);
            let mut c = [10.0 + 22.0 * t, 14.0 + 4.0 * t, 30.0 + 18.0 * t];
            let blob = |cx: f32, cy: f32, r: f32| {
                (1.0 - ((u - cx).hypot((v - cy) * 0.6) / r))
                    .clamp(0.0, 1.0)
                    .powi(2)
            };
            let a = blob(0.2, 0.25, 0.5);
            let b = blob(0.85, 0.8, 0.45);
            c[0] += 16.0 * a + 40.0 * b;
            c[1] += 40.0 * a + 28.0 * b;
            c[2] += 52.0 * a + 4.0 * b;
            let i = ((y * w + x) * 4) as usize;
            im.px[i] = c[0] as u8;
            im.px[i + 1] = c[1] as u8;
            im.px[i + 2] = c[2] as u8;
            im.px[i + 3] = 255;
        }
    }
    im
}

fn upload(r: &mut GlesRenderer, rgba: &[u8], w: i32, h: i32) -> Result<GlesTexture, String> {
    r.import_memory(rgba, Fourcc::Abgr8888, (w, h).into(), false)
        .map_err(|e| format!("uploading {w}x{h}: {e}"))
}

// ---------------------------------------------------------------- playing

struct Play {
    registry: ShaderRegistry,
    anims: Animations,
    kind: RunKind,
    style: String,
    travel: (f64, f64),
}

impl Play {
    fn new(
        styles: Vec<transitions::TransitionStyle>,
        style: &str,
        kind: RunKind,
        travel: (f64, f64),
    ) -> Result<Self, String> {
        let mut registry = ShaderRegistry::default();
        registry.set_catalog(styles);
        let mut anims = Animations {
            preset: Preset::Smooth,
            ..Animations::default()
        };
        anims.overrides.insert(
            kind.event(),
            Override {
                style: Some(style.to_owned()),
                ..Default::default()
            },
        );
        registry.select(&anims);
        Ok(Play {
            registry,
            anims,
            kind,
            style: style.to_owned(),
            travel,
        })
    }
}

struct Rendered {
    /// Straight RGBA rows, top first.
    frames: Vec<Image>,
    duration: Duration,
    /// Per-frame GPU ms (glFinish-timed), `element()` through the draw.
    ms: Vec<f64>,
}

/// Play the style at `win` over `frames` steps of progress 0 to 1. `read`
/// copies each composited frame back; timing runs skip it.
fn play(
    r: &mut GlesRenderer,
    play: &mut Play,
    win: (i32, i32),
    frames: usize,
    read: bool,
) -> Result<Rendered, String> {
    let scale = Scale::from(1.0);
    let win_l: Size<i32, Logical> = win.into();
    let win_p: Size<i32, Physical> = win.into();
    let t0 = Instant::now();
    play.registry.compile_wanted(r);
    let run = play
        .registry
        .start(&play.anims, play.kind, t0, ShedLevel::Full, play.travel, 0.0)
        .ok_or_else(|| {
            format!(
                "`{}` does not serve {} (or is not installed / failed to compile; see the log above)",
                play.style,
                play.kind.event().key()
            )
        })?;
    if !play.registry.ready(&run.style) {
        return Err(format!(
            "`{}` failed to compile (see the warning above)",
            run.style
        ));
    }
    let quad = run.quad(win_l, scale);
    let size = quad.size(win_p);
    let ctx = r.context_id();
    let canvas_rect = Rectangle::<i32, Physical>::from_size(size);

    let sample = sample_window(win.0, win.1);
    let window_tex = upload(r, &sample.premultiplied(), win.0, win.1)?;
    let wall = backdrop(size.w, size.h);
    let wall_tex = upload(r, &wall.premultiplied(), size.w, size.h)?;
    let mut canvas = Offscreen::<GlesTexture>::create_buffer(r, Fourcc::Abgr8888, (size.w, size.h).into())
        .map_err(|e| format!("canvas: {e}"))?;
    let id = Id::new();

    let mut out = Vec::new();
    let mut ms = Vec::new();
    for i in 0..frames {
        let t = i as f64 / (frames - 1) as f64;
        let now = t0 + run.leg.duration.mul_f64(t);
        let started = Instant::now();
        let tex = window_tex.clone();
        let (ctx2, id2, origin) = (ctx.clone(), id.clone(), quad.origin());
        let element = play
            .registry
            .element(
                r,
                ctx.clone(),
                &run,
                now,
                win_p,
                quad,
                (0, 0).into(),
                scale,
                move |_| {
                    vec![TextureRenderElement::from_static_texture(
                        id2,
                        ctx2,
                        origin.to_f64(),
                        tex,
                        1,
                        Transform::Normal,
                        None,
                        None,
                        Some(win_l),
                        None,
                        Kind::Unspecified,
                    )]
                },
            )
            .ok_or_else(|| "the shader could not draw (see the log above)".to_string())?;
        let mut fb = Bind::bind(r, &mut canvas).map_err(|e| format!("bind: {e}"))?;
        {
            let mut frame = r
                .render(&mut fb, size, Transform::Normal)
                .map_err(|e| format!("render: {e}"))?;
            frame
                .clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[canvas_rect])
                .map_err(|e| e.to_string())?;
            frame
                .render_texture_from_to(
                    &wall_tex,
                    Rectangle::<f64, Buffer>::from_size((size.w as f64, size.h as f64).into()),
                    canvas_rect,
                    &[canvas_rect],
                    &[canvas_rect],
                    Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
                .map_err(|e| e.to_string())?;
            let geo = element.geometry(scale);
            RenderElement::draw(
                &element,
                &mut frame,
                element.src(),
                geo,
                &[Rectangle::from_size(geo.size)],
                &[],
            )
            .map_err(|e| e.to_string())?;
            let sync = frame.finish().map_err(|e| e.to_string())?;
            let _ = sync.wait();
        }
        r.with_context(|gl| unsafe { gl.Finish() })
            .map_err(|e| e.to_string())?;
        ms.push(started.elapsed().as_secs_f64() * 1000.0);
        if read {
            let region = Rectangle::<i32, Buffer>::from_size((size.w, size.h).into());
            let mapping = r
                .copy_framebuffer(&fb, region, Fourcc::Abgr8888)
                .map_err(|e| format!("readback: {e}"))?;
            // GL rows run bottom-up; smithay reports `flipped` for the already-upright case.
            let flipped = !mapping.flipped();
            let bytes = r.map_texture(&mapping).map_err(|e| format!("map: {e}"))?;
            let mut im = Image::new(size.w, size.h);
            let stride = (size.w * 4) as usize;
            for y in 0..size.h as usize {
                let src = if flipped { size.h as usize - 1 - y } else { y };
                im.px[y * stride..(y + 1) * stride].copy_from_slice(&bytes[src * stride..(src + 1) * stride]);
            }
            out.push(im);
        }
    }
    Ok(Rendered {
        frames: out,
        duration: run.leg.duration,
        ms,
    })
}

// ---------------------------------------------------------------- encoders

fn downscale(src: &Image, tw: i32, th: i32) -> Image {
    let mut dst = Image::new(tw, th);
    for y in 0..th {
        let (sy0, sy1) = (
            (y * src.h / th) as usize,
            (((y + 1) * src.h / th).max(y * src.h / th + 1)) as usize,
        );
        for x in 0..tw {
            let (sx0, sx1) = (
                (x * src.w / tw) as usize,
                (((x + 1) * src.w / tw).max(x * src.w / tw + 1)) as usize,
            );
            let mut acc = [0u32; 4];
            let mut n = 0;
            for sy in sy0..sy1.min(src.h as usize) {
                for sx in sx0..sx1.min(src.w as usize) {
                    let i = (sy * src.w as usize + sx) * 4;
                    for k in 0..4 {
                        acc[k] += src.px[i + k] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((y * tw + x) * 4) as usize;
            for k in 0..4 {
                dst.px[o + k] = (acc[k] / n.max(1)) as u8;
            }
        }
    }
    dst
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// An RGB PNG with stored (uncompressed) deflate blocks: no dependency.
fn png(im: &Image) -> Vec<u8> {
    let mut raw = Vec::with_capacity((im.w * im.h * 3 + im.h) as usize);
    for y in 0..im.h as usize {
        raw.push(0);
        for x in 0..im.w as usize {
            raw.extend_from_slice(&im.px[(y * im.w as usize + x) * 4..][..3]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |tag: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = tag.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(im.w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(im.h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// A 256-colour palette by median cut over a sample of every frame.
fn palette(frames: &[Image]) -> Vec<[u8; 3]> {
    let mut colours: Vec<[u8; 3]> = Vec::new();
    for f in frames {
        for p in f.px.chunks_exact(4).step_by(5) {
            colours.push([p[0], p[1], p[2]]);
        }
    }
    let mut boxes = vec![colours];
    while boxes.len() < 256 {
        // Split the box with the widest channel range.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let (ch, range) = (0..3)
                    .map(|c| {
                        let (lo, hi) = b
                            .iter()
                            .fold((255u8, 0u8), |(l, h), p| (l.min(p[c]), h.max(p[c])));
                        (c, hi - lo)
                    })
                    .max_by_key(|(_, r)| *r)
                    .unwrap();
                (i, ch, range)
            })
            .max_by_key(|(_, _, r)| *r);
        let Some((i, ch, range)) = pick else { break };
        if range == 0 {
            break;
        }
        let mut b = boxes.swap_remove(i);
        b.sort_unstable_by_key(|p| p[ch]);
        let hi = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(hi);
    }
    let mut pal: Vec<[u8; 3]> = boxes
        .iter()
        .map(|b| {
            let n = b.len().max(1) as u32;
            let s = b.iter().fold([0u32; 3], |mut s, p| {
                for k in 0..3 {
                    s[k] += p[k] as u32;
                }
                s
            });
            [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8]
        })
        .collect();
    pal.resize(256, [0, 0, 0]);
    pal
}

fn lzw(indices: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut nbits) = (0u32, 0u32);
    let mut emit = |code: u32, size: u32, out: &mut Vec<u8>| {
        acc |= code << nbits;
        nbits += size;
        while nbits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            nbits -= 8;
        }
    };
    let mut dict: HashMap<(u16, u8), u16> = HashMap::new();
    let (mut next, mut size) = (258u32, 9u32);
    emit(256, size, &mut out);
    let mut cur = indices[0] as u16;
    for &b in &indices[1..] {
        if let Some(&c) = dict.get(&(cur, b)) {
            cur = c;
            continue;
        }
        emit(cur as u32, size, &mut out);
        if next < 4096 {
            dict.insert((cur, b), next as u16);
            if next == (1 << size) && size < 12 {
                size += 1;
            }
            next += 1;
        } else {
            emit(256, size, &mut out);
            dict.clear();
            next = 258;
            size = 9;
        }
        cur = b as u16;
    }
    emit(cur as u32, size, &mut out);
    emit(257, size, &mut out);
    if nbits > 0 {
        out.push(acc as u8);
    }
    out
}

fn gif(frames: &[Image], delay_cs: u16, hold_cs: u16) -> Vec<u8> {
    let (w, h) = (frames[0].w as u16, frames[0].h as u16);
    let pal = palette(frames);
    // Nearest palette entry per 5-5-5 colour, found lazily.
    let mut lut = vec![u16::MAX; 32768];
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&[0xF7, 0, 0]);
    for c in &pal {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&[0x21, 0xFF, 0x0B]);
    out.extend_from_slice(b"NETSCAPE2.0");
    out.extend_from_slice(&[3, 1, 0, 0, 0]);
    for (n, f) in frames.iter().enumerate() {
        let delay = if n + 1 == frames.len() { hold_cs } else { delay_cs };
        out.extend_from_slice(&[0x21, 0xF9, 4, 0]);
        out.extend_from_slice(&delay.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.push(0x2C);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.push(0);
        let idx: Vec<u8> = f
            .px
            .chunks_exact(4)
            .map(|p| {
                let key = ((p[0] as usize >> 3) << 10) | ((p[1] as usize >> 3) << 5) | (p[2] as usize >> 3);
                if lut[key] == u16::MAX {
                    let c = [
                        (p[0] & 0xF8) as i32 + 4,
                        (p[1] & 0xF8) as i32 + 4,
                        (p[2] & 0xF8) as i32 + 4,
                    ];
                    lut[key] = pal
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, q)| (0..3).map(|k| (q[k] as i32 - c[k]).pow(2)).sum::<i32>())
                        .map(|(i, _)| i as u16)
                        .unwrap_or(0);
                }
                lut[key] as u8
            })
            .collect();
        out.push(8);
        for block in lzw(&idx).chunks(255) {
            out.push(block.len() as u8);
            out.extend_from_slice(block);
        }
        out.push(0);
    }
    out.push(0x3B);
    out
}

/// Tile the frames, `cols` across, each scaled to `tw` wide.
fn contact_sheet(frames: &[Image], cols: usize, tw: i32) -> Image {
    let th = tw * frames[0].h / frames[0].w;
    let rows = frames.len().div_ceil(cols);
    let gap = 4;
    let mut sheet = Image::new(cols as i32 * (tw + gap) + gap, rows as i32 * (th + gap) + gap);
    for p in sheet.px.chunks_exact_mut(4) {
        p.copy_from_slice(&[6, 7, 10, 255]);
    }
    for (n, f) in frames.iter().enumerate() {
        let t = downscale(f, tw, th);
        let (ox, oy) = (
            (n % cols) as i32 * (tw + gap) + gap,
            (n / cols) as i32 * (th + gap) + gap,
        );
        for y in 0..th {
            let d = (((oy + y) * sheet.w + ox) * 4) as usize;
            let s = (y * tw * 4) as usize;
            sheet.px[d..d + (tw * 4) as usize].copy_from_slice(&t.px[s..s + (tw * 4) as usize]);
        }
    }
    sheet
}

// ---------------------------------------------------------------- main

fn find_style(styles: &[transitions::TransitionStyle], name: &str) -> Result<String, String> {
    if styles.iter().any(|s| s.id == name) {
        return Ok(name.to_owned());
    }
    let tail = format!(":{name}");
    let hits: Vec<&str> = styles
        .iter()
        .map(|s| s.id.as_str())
        .filter(|id| id.ends_with(&tail))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(format!(
            "no style `{name}`; installed: {}",
            styles
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        many => Err(format!("`{name}` is ambiguous: {}", many.join(", "))),
    }
}

/// `--catalog` is the transitions root (`<pack>/<style>.kdl`); a single pack
/// directory is wrapped so it loads as `<pack>:<style>` too.
fn load(catalog: &Path) -> transitions::Catalog {
    let direct = transitions::load_from(catalog);
    if !direct.styles.is_empty() {
        return direct;
    }
    match catalog.parent() {
        Some(parent)
            if catalog.is_dir()
                && catalog.read_dir().is_ok_and(|mut d| {
                    d.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "kdl")))
                }) =>
        {
            let all = transitions::load_from(parent);
            let name = catalog
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            transitions::Catalog {
                styles: all
                    .styles
                    .into_iter()
                    .filter(|s| s.id.starts_with(&format!("{name}:")))
                    .collect(),
                ..all
            }
        }
        _ => direct,
    }
}

fn run() -> Result<(), String> {
    let a = parse_args();
    let catalog = load(&a.catalog);
    for r in &catalog.rejected {
        eprintln!("rejected {}: {}", r.path.display(), r.reason);
    }
    let id = find_style(&catalog.styles, &a.style)?;
    let travel = a.travel.unwrap_or(match a.kind {
        RunKind::Minimize => (0.0, a.size.1 as f64),
        _ => (0.0, 0.0),
    });
    let mut r = renderer()?;
    let ev: Event = a.kind.event();
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;

    let mut p = Play::new(catalog.styles.clone(), &id, a.kind, travel)?;
    let shown = play(&mut r, &mut p, a.size, a.frames, true)?;
    let stem = format!("{}-{}", id.replace(':', "_"), ev.key());
    let sheet = contact_sheet(&shown.frames, 6, 320);
    let png_path = a.out.join(format!("{stem}.png"));
    std::fs::write(&png_path, png(&sheet)).map_err(|e| e.to_string())?;
    let gif_w = 640.min(shown.frames[0].w);
    let small: Vec<Image> = shown
        .frames
        .iter()
        .map(|f| downscale(f, gif_w, gif_w * f.h / f.w))
        .collect();
    let delay = ((shown.duration.as_millis() as f64 / a.frames as f64) / 10.0)
        .round()
        .max(2.0) as u16;
    let gif_path = a.out.join(format!("{stem}.gif"));
    std::fs::write(&gif_path, gif(&small, delay, 100)).map_err(|e| e.to_string())?;

    // Timing: a fresh registry and run at the 4K-class window, no readback.
    let mut p = Play::new(
        catalog.styles.clone(),
        &id,
        a.kind,
        (
            travel.0 * TIMING_SIZE.0 as f64 / a.size.0 as f64,
            travel.1 * TIMING_SIZE.1 as f64 / a.size.1 as f64,
        ),
    )?;
    let timed = play(&mut r, &mut p, TIMING_SIZE, a.frames, false)?;
    let steady = &timed.ms[1..];
    let avg = steady.iter().sum::<f64>() / steady.len() as f64;
    let worst = steady.iter().cloned().fold(0.0, f64::max);

    println!("style    {id} ({})", ev.key());
    println!("sheet    {}", png_path.display());
    println!("gif      {}", gif_path.display());
    println!(
        "gpu      {avg:.2} ms/frame avg (max {worst:.2}, first {:.2}) at {}x{}, {} frames",
        timed.ms[0], TIMING_SIZE.0, TIMING_SIZE.1, a.frames
    );
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("shader-preview: {e}");
        std::process::exit(1);
    }
}
