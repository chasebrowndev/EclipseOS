// SPDX-License-Identifier: AGPL-3.0-only
//! The compositor-drawn wallpaper, state side (ADR 0104 stage 2; COMP-02).
//!
//! Config is the `wallpaper { path mode color output "<name>" { .. } }` node
//! `ec-wallpaper` always read; the keys and their meaning are unchanged. Each
//! frame [`tick`] works out what every output should show and, when that has
//! moved, hands the decode and layout to one short-lived worker thread (the
//! single-threaded core never blocks on a file). The worker answers by
//! channel with an output-sized pixel buffer, which [`ec_abyss_render::wallpaper::Store`]
//! draws as the bottom-most element. Until it lands, and if the file will not
//! decode, the output shows its colour alone.
//!
//! Classification: the wallpaper is compositor chrome with no client behind
//! it, so it is `public` and is not a surface the capture path ever lists.
//! `render/capture.rs` builds its own pass from window surfaces and never
//! calls `collect_elements`, so a capture does not contain it either.
//!
//! No human input is read here, and the path is configuration, not content.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ec_abyss_render::wallpaper::{compose, Image, Key, Mode};
use smithay::{
    output::Output,
    reexports::calloop::channel::{self, Event},
    utils::Scale,
};

use crate::config::{schema::WALLPAPER_DEFAULT_COLOR, Wallpaper};
use crate::state::AbyssState;

/// A picture larger than this many pixels is refused rather than decoded.
const MAX_PIXELS: u64 = 160_000_000;

/// What the worker tells the loop.
pub enum Msg {
    /// Job `index` finished: the composed buffer and its hash, or nothing.
    Built {
        index: usize,
        pixels: Option<(Vec<u8>, [u8; 32])>,
    },
    /// The batch is over (also sent if the worker dies).
    Done,
}

/// The loop's side of the worker.
#[derive(Default)]
pub struct Loader {
    tx: Option<channel::Sender<Msg>>,
    busy: bool,
    /// The jobs in flight, indexed by `Msg::Built::index`.
    batch: Vec<(Output, Key)>,
    /// Signature of the inputs last fully handled; `None` forces a pass.
    sig: Option<u64>,
    changed: bool,
}

/// One output's wallpaper as the config resolves it: picture, mode, colour.
pub fn resolve(w: &Wallpaper, name: &str, theme_base: [f32; 4]) -> (Option<PathBuf>, Mode, [f32; 4]) {
    let over = w.outputs.iter().find(|o| o.name == name);
    let path = over
        .and_then(|o| o.path.as_deref())
        .or(w.path.as_deref())
        .filter(|p| !p.trim().is_empty())
        .map(expand);
    let mode = Mode::parse(over.and_then(|o| o.mode.as_deref()).unwrap_or(&w.mode));
    // A global colour still at the built-in default is "unset": the ground
    // follows the theme's base, so the light scheme is not pinned dark.
    let global = if w.color == WALLPAPER_DEFAULT_COLOR {
        theme_base
    } else {
        w.color
    };
    let color = over.and_then(|o| o.color).unwrap_or(global);
    (path, mode, [color[0], color[1], color[2], 1.0])
}

/// A leading `~` is `$HOME`. Nothing else is touched.
fn expand(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home,
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

fn byte(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Called once per frame before the elements are collected. Cheap when
/// nothing moved: one hash over the inputs.
pub fn tick(state: &mut AbyssState) {
    let theme_base = state.theme.appearance().seeds.base.0;
    let epoch = state.borders.wallpaper.epoch();
    let outs: Vec<Output> = state.space.outputs().cloned().collect();

    let mut h = std::collections::hash_map::DefaultHasher::new();
    epoch.hash(&mut h);
    theme_base.map(f32::to_bits).hash(&mut h);
    for o in &outs {
        o.name().hash(&mut h);
        o.current_scale().fractional_scale().to_bits().hash(&mut h);
        if let Some(g) = state.space.output_geometry(o) {
            (g.size.w, g.size.h).hash(&mut h);
        }
    }
    let w = &state.config.wallpaper;
    (&w.path, &w.mode, w.color.map(f32::to_bits)).hash(&mut h);
    for o in &w.outputs {
        (&o.name, &o.path, &o.mode, o.color.map(|c| c.map(f32::to_bits))).hash(&mut h);
    }
    let sig = h.finish();
    if state.wallpaper.sig == Some(sig) {
        return;
    }

    state.borders.wallpaper.retain(&outs);
    let mut todo: Vec<(Output, Key)> = Vec::new();
    for o in &outs {
        let Some(geo) = state.space.output_geometry(o) else {
            continue;
        };
        let scale = o.current_scale().fractional_scale();
        let phys = geo
            .size
            .to_f64()
            .to_physical(Scale::from(scale))
            .to_i32_round::<i32>();
        let (path, mode, color) = resolve(&state.config.wallpaper, &o.name(), theme_base);
        state.borders.wallpaper.want(o, geo.size, color);
        let key = Key {
            path,
            mode,
            color: [byte(color[0]), byte(color[1]), byte(color[2])],
            size: (phys.w.max(0) as u32, phys.h.max(0) as u32),
            scale_milli: (scale * 1000.0).round() as u32,
            epoch,
        };
        if state.borders.wallpaper.satisfied(o, &key) {
            continue;
        }
        if key.path.is_none() {
            // The colour alone: nothing to decode.
            state.borders.wallpaper.install(o, key, None);
        } else {
            todo.push((o.clone(), key));
        }
    }
    if todo.is_empty() {
        state.wallpaper.sig = Some(sig);
        return;
    }
    if state.wallpaper.busy {
        // Picked up on a later frame once the running batch reports Done.
        return;
    }
    spawn(state, todo);
    state.wallpaper.sig = Some(sig);
}

fn spawn(state: &mut AbyssState, todo: Vec<(Output, Key)>) {
    if state.wallpaper.tx.is_none() {
        let (tx, rx) = channel::channel::<Msg>();
        let inserted = state.loop_handle.insert_source(rx, |event, _, state| {
            if let Event::Msg(msg) = event {
                arrived(state, msg);
            }
        });
        if let Err(e) = inserted {
            tracing::warn!(error = %e, "wallpaper: could not queue the loader");
            return;
        }
        state.wallpaper.tx = Some(tx);
    }
    let Some(tx) = state.wallpaper.tx.clone() else {
        return;
    };
    let jobs: Vec<Job> = todo
        .iter()
        .map(|(_, k)| Job {
            path: k.path.clone().unwrap_or_default(),
            mode: k.mode,
            color: k.color,
            size: k.size,
            scale: k.scale_milli as f64 / 1000.0,
        })
        .collect();
    state.wallpaper.batch = todo;
    state.wallpaper.busy = true;
    state.wallpaper.changed = false;
    let spawned = std::thread::Builder::new()
        .name("ec-wallpaper".into())
        .spawn(move || work(jobs, tx));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "wallpaper: could not start the loader");
        state.wallpaper.busy = false;
    }
}

struct Job {
    path: PathBuf,
    mode: Mode,
    color: [u8; 3],
    size: (u32, u32),
    scale: f64,
}

/// Reports `Done` however the worker ends, a panic included.
struct Finish(channel::Sender<Msg>);

impl Drop for Finish {
    fn drop(&mut self) {
        let _ = self.0.send(Msg::Done);
    }
}

fn work(jobs: Vec<Job>, tx: channel::Sender<Msg>) {
    let finish = Finish(tx);
    // Each distinct picture is decoded once per batch, however many outputs
    // show it, and dropped with the batch.
    let mut decoded: HashMap<PathBuf, Option<Arc<Image>>> = HashMap::new();
    for (index, job) in jobs.iter().enumerate() {
        let img = decoded
            .entry(job.path.clone())
            .or_insert_with(|| match decode(&job.path) {
                Ok(img) => Some(Arc::new(img)),
                Err(e) => {
                    tracing::warn!(path = %job.path.display(), error = %e, "wallpaper: not drawn, colour only");
                    None
                }
            })
            .clone();
        let pixels = img.map(|img| {
            let bytes = compose(&img, job.size, job.mode, job.scale, job.color);
            let hash = *blake3::hash(&bytes).as_bytes();
            (bytes, hash)
        });
        let _ = finish.0.send(Msg::Built { index, pixels });
    }
}

fn arrived(state: &mut AbyssState, msg: Msg) {
    match msg {
        Msg::Built { index, pixels } => {
            let Some((output, key)) = state.wallpaper.batch.get(index).cloned() else {
                return;
            };
            if state.borders.wallpaper.install(&output, key, pixels) {
                state.wallpaper.changed = true;
            }
        }
        Msg::Done => {
            state.wallpaper.busy = false;
            state.wallpaper.batch.clear();
            // Anything that moved while the batch ran gets its pass.
            state.wallpaper.sig = None;
            if std::mem::take(&mut state.wallpaper.changed) {
                crate::backend::damage_all(state);
            }
        }
    }
}

/// Read and decode a PNG or JPEG into RGBA8.
pub fn decode(path: &Path) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.starts_with(b"\x89PNG") {
        decode_png(&bytes)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        decode_jpeg(&bytes)
    } else {
        Err("not a PNG or JPEG".into())
    }
}

fn decode_png(bytes: &[u8]) -> Result<Image, String> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if u64::from(info.width) * u64::from(info.height) > MAX_PIXELS {
        return Err("picture too large".into());
    }
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or("picture too large")?];
    let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(frame.buffer_size());
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    match frame.color_type {
        png::ColorType::Rgba => rgba = buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .for_each(|p| rgba.extend_from_slice(&[p[0], p[1], p[2], 255])),
        png::ColorType::Grayscale => buf.iter().for_each(|&g| rgba.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .for_each(|p| rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
        png::ColorType::Indexed => return Err("indexed PNG left unexpanded".into()),
    }
    if rgba.len() != w * h * 4 {
        return Err("short PNG data".into());
    }
    Ok(Image {
        width: frame.width,
        height: frame.height,
        rgba,
    })
}

fn decode_jpeg(bytes: &[u8]) -> Result<Image, String> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    dec.decode_headers().map_err(|e| format!("{e:?}"))?;
    let info = dec.info().ok_or("no JPEG header")?;
    if u64::from(info.width) * u64::from(info.height) > MAX_PIXELS {
        return Err("picture too large".into());
    }
    let rgba = dec.decode().map_err(|e| format!("{e:?}"))?;
    if rgba.len() != info.width as usize * info.height as usize * 4 {
        return Err("short JPEG data".into());
    }
    Ok(Image {
        width: u32::from(info.width),
        height: u32::from(info.height),
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WallpaperOutput;

    const BASE: [f32; 4] = [0.9, 0.9, 0.9, 1.0];

    fn wp() -> Wallpaper {
        Wallpaper::default()
    }

    #[test]
    fn defaults_are_the_theme_base_and_no_picture() {
        let (path, mode, color) = resolve(&wp(), "DP-1", BASE);
        assert_eq!((path, mode, color), (None, Mode::Fill, [0.9, 0.9, 0.9, 1.0]));
    }

    #[test]
    fn a_set_colour_beats_the_theme_and_an_override_beats_both() {
        let mut w = wp();
        w.color = [0.1, 0.2, 0.3, 1.0];
        w.outputs.push(WallpaperOutput {
            name: "DP-2".into(),
            path: Some("/x.png".into()),
            mode: Some("fit".into()),
            color: Some([0.5, 0.5, 0.5, 1.0]),
        });
        assert_eq!(resolve(&w, "DP-1", BASE).2, [0.1, 0.2, 0.3, 1.0]);
        let (path, mode, color) = resolve(&w, "DP-2", BASE);
        assert_eq!(
            (path, mode, color),
            (Some("/x.png".into()), Mode::Fit, [0.5, 0.5, 0.5, 1.0])
        );
    }

    #[test]
    fn an_empty_override_path_means_no_picture_and_tilde_is_home() {
        let mut w = wp();
        w.path = Some("~/a.png".into());
        w.outputs.push(WallpaperOutput {
            name: "DP-2".into(),
            path: Some(String::new()),
            ..WallpaperOutput::default()
        });
        assert_eq!(resolve(&w, "DP-2", BASE).0, None);
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        assert_eq!(resolve(&w, "DP-1", BASE).0, Some(home.join("a.png")));
    }

    #[test]
    fn a_png_round_trips_through_the_decoder() {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 2, 1);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&[1, 2, 3, 4, 5, 6])
            .unwrap();
        let img = decode_png(&out).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn junk_is_refused_not_panicked_on() {
        assert!(decode_png(b"\x89PNG nonsense").is_err());
        assert!(decode_jpeg(&[0xff, 0xd8, 0, 0]).is_err());
    }
}
