// SPDX-License-Identifier: AGPL-3.0-only
//! Presenters (T-07 §3, §3.1; ADR 0119; RENDERING-07, -59, -61, -62, -63, -64, -68;
//! PERFORMANCE-76).
//!
//! The product presenter is the wgpu surface ([`SurfacePresenter`]); the offscreen presenter
//! ([`OffscreenPresenter`]) is the T-25 test path. There is no `wl_shm` or other presenter.
//!
//! # Unsafe
//!
//! This crate is `forbid(unsafe_code)` (BRIEF rule 6). The surface is created from a safe
//! `HasWindowHandle + HasDisplayHandle` value ([`WindowTarget`]) handed in by `apoth-wl`,
//! which builds the raw Wayland handles from its display and surface pointers (T-09 §7) and so
//! owns the `unsafe` and its `// SAFETY:` comment. `apoth-render` never touches a raw
//! pointer.
//!
//! # Commit ownership (T-07 §3.1)
//!
//! wgpu commits the Wayland surface inside `Queue::present` on the render thread. The
//! requests that must precede that commit (pending `ack_configure`, geometry and regions,
//! `wl_surface.frame` when the gate is on, then `wp_presentation.feedback`) are sent by the
//! hook installed with [`SurfacePresenter::set_pre_present`], which `apoth-wl` supplies; it
//! runs after the frame is recorded and submitted and immediately before `present`.
//!
//! # Blocking
//!
//! Under `Mailbox` acquire and present do not block. Under `Fifo`, `get_current_texture` may
//! block the render thread; that is the one permitted blocking GPU wait and the frame gate is
//! off in that mode (see `sched`). Nothing here is ever called from the main thread.
//!
//! # Timings
//!
//! [`FrameStats::returned_at`] is the instant `present()` returned: the end stamp of
//! `LAT-ECHO` (T-23). The caller emits the tracing span (sizes and durations only); this crate
//! has no tracing dependency.

use crate::gpu::{AdapterReport, Gpu, GpuOptions, RenderError, new_instance};
use crate::reference::Image;
use crate::renderer::{DrawStats, FrameInput, Renderer};
use crate::surface::{
    AdapterClass, AlphaKind, AlphaRequest, FRAME_LATENCY, PresentKind, SurfaceFault,
    choose_present_mode, resolve_alpha,
};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A window the surface can be created on: safe window and display handles. Implemented by
/// `apoth-wl`'s surface wrapper; the blanket impl covers anything with the handle traits.
pub trait WindowTarget:
    wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + Send + Sync + 'static
{
}

impl<T> WindowTarget for T where
    T: wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + Send + Sync + 'static
{
}

/// What `apoth doctor` reports about presentation.
#[derive(Clone, Debug)]
pub struct PresentInfo {
    /// Present mode in use.
    pub present_mode: PresentKind,
    /// Composite alpha mode in use.
    pub alpha: AlphaKind,
    /// Premultiplied alpha is unavailable and the surface is opaque.
    pub opaque_fallback: bool,
    /// Surface format.
    pub format: wgpu::TextureFormat,
    /// The adapter.
    pub adapter: AdapterReport,
}

/// Timings and work of one presented frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameStats {
    /// What the executor drew.
    pub draw: DrawStats,
    /// Time from acquire to submit.
    pub record: Duration,
    /// Time inside `Queue::present`.
    pub present_call: Duration,
    /// When `present()` returned (end of `LAT-ECHO`).
    pub returned_at: Instant,
}

/// The result of one present attempt.
#[derive(Clone, Copy, Debug)]
pub enum PresentOutcome {
    /// Drawn and presented.
    Presented(FrameStats),
    /// Nothing was presented; the renderer was invalidated so the next frame redraws fully.
    /// The caller retries on the next damage or frame callback.
    Skipped(SurfaceFault),
    /// The device was lost: rebuild [`Gpu`], [`Renderer`] and the atlases (PERFORMANCE-76).
    DeviceLost,
}

/// Something frames are presented to.
pub trait Presenter {
    /// The colour format the executor must target.
    fn format(&self) -> wgpu::TextureFormat;
    /// Current size in pixels.
    fn size(&self) -> (u32, u32);
    /// The window changed size.
    fn resize(&mut self, size: (u32, u32));
    /// Present mode, alpha and adapter.
    fn info(&self) -> PresentInfo;
    /// Records the frame, submits it and presents.
    fn present(&mut self, renderer: &mut Renderer, input: &FrameInput<'_>) -> PresentOutcome;
}

/// Renders into an owned RGBA8 texture that can be read back (goldens, benches).
pub struct OffscreenPresenter {
    gpu: Gpu,
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl fmt::Debug for OffscreenPresenter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OffscreenPresenter")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl OffscreenPresenter {
    /// A `w` x `h` offscreen target.
    #[must_use]
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Self {
        let (tex, view) = Self::make(gpu, size);
        Self {
            gpu: gpu.clone(),
            tex,
            view,
            size,
        }
    }

    fn make(gpu: &Gpu, size: (u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-target"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    /// Reads the target back as a premultiplied RGBA8 image. This waits for the GPU; it is
    /// the test path and is never used by a window.
    ///
    /// # Panics
    /// If the device is lost or mapping fails.
    #[must_use]
    pub fn read_pixels(&self) -> Image {
        let (w, h) = (self.size.0.max(1), self.size.1.max(1));
        let stride = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(stride) * u64::from(h),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::default(),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        rx.recv().expect("map callback").expect("buffer map");
        let mut data = vec![0u8; (w * h * 4) as usize];
        {
            let view = buf.slice(..).get_mapped_range().expect("mapped range");
            for y in 0..h as usize {
                let src = y * stride as usize;
                let dst = y * w as usize * 4;
                data[dst..dst + w as usize * 4].copy_from_slice(&view[src..src + w as usize * 4]);
            }
        }
        buf.unmap();
        Image {
            w: w as usize,
            h: h as usize,
            data,
        }
    }
}

impl Presenter for OffscreenPresenter {
    fn format(&self) -> wgpu::TextureFormat {
        OFFSCREEN_FORMAT
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn resize(&mut self, size: (u32, u32)) {
        if size != self.size {
            self.size = size;
            let (tex, view) = Self::make(&self.gpu, size);
            self.tex = tex;
            self.view = view;
        }
    }

    fn info(&self) -> PresentInfo {
        PresentInfo {
            present_mode: PresentKind::Other,
            alpha: AlphaKind::PreMultiplied,
            opaque_fallback: false,
            format: OFFSCREEN_FORMAT,
            adapter: self.gpu.report().clone(),
        }
    }

    fn present(&mut self, renderer: &mut Renderer, input: &FrameInput<'_>) -> PresentOutcome {
        if self.gpu.is_lost() {
            renderer.invalidate_all();
            return PresentOutcome::DeviceLost;
        }
        let t0 = Instant::now();
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let draw = renderer.draw(&mut enc, &self.view, self.size, input);
        self.gpu.queue.submit(std::iter::once(enc.finish()));
        let record = t0.elapsed();
        PresentOutcome::Presented(FrameStats {
            draw,
            record,
            present_call: Duration::ZERO,
            returned_at: Instant::now(),
        })
    }
}

/// Options for [`SurfacePresenter::new`].
#[derive(Clone, Debug, Default)]
pub struct SurfaceOptions {
    /// Adapter selection.
    pub gpu: GpuOptions,
    /// Composite alpha request (`Auto` substitutes, an explicit mode must be offered).
    pub alpha: AlphaRequest,
}

fn present_kind(m: wgpu::PresentMode) -> PresentKind {
    match m {
        wgpu::PresentMode::Mailbox => PresentKind::Mailbox,
        wgpu::PresentMode::Fifo => PresentKind::Fifo,
        _ => PresentKind::Other,
    }
}

fn alpha_kind(m: wgpu::CompositeAlphaMode) -> AlphaKind {
    match m {
        wgpu::CompositeAlphaMode::Opaque => AlphaKind::Opaque,
        wgpu::CompositeAlphaMode::PreMultiplied => AlphaKind::PreMultiplied,
        _ => AlphaKind::Other,
    }
}

/// The wgpu surface presenter.
pub struct SurfacePresenter {
    gpu: Gpu,
    target: Arc<dyn WindowTarget>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    info: PresentInfo,
    size: (u32, u32),
    needs_reconfigure: bool,
    pre_present: Option<Box<dyn FnMut() + Send>>,
}

impl fmt::Debug for SurfacePresenter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurfacePresenter")
            .field("size", &self.size)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl SurfacePresenter {
    /// Creates the surface on `target`, picks the adapter, opens the device and configures
    /// the swapchain: `Mailbox` if offered else `Fifo`, premultiplied alpha if offered, one
    /// frame of latency, a non-sRGB format (all blending is done in the shader).
    ///
    /// # Errors
    /// [`RenderError`] when no adapter can present, the surface offers no format, or an
    /// explicit alpha mode is unsupported.
    pub fn new<W: WindowTarget>(
        target: Arc<W>,
        size: (u32, u32),
        opts: &SurfaceOptions,
    ) -> Result<Self, RenderError> {
        let target: Arc<dyn WindowTarget> = target;
        let instance = new_instance();
        let surface = instance
            .create_surface(Arc::clone(&target))
            .map_err(|e| RenderError::Surface(format!("{e:?}")))?;
        let gpu = Gpu::from_instance(instance, &opts.gpu, Some(&surface))?;
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or(RenderError::NoSurfaceFormat)?;
        let modes: Vec<PresentKind> = caps
            .present_modes
            .iter()
            .map(|m| present_kind(*m))
            .collect();
        let present_mode = match choose_present_mode(&modes) {
            PresentKind::Mailbox => wgpu::PresentMode::Mailbox,
            _ => wgpu::PresentMode::Fifo,
        };
        let alphas: Vec<AlphaKind> = caps.alpha_modes.iter().map(|m| alpha_kind(*m)).collect();
        let choice =
            resolve_alpha(&alphas, opts.alpha).map_err(|_| RenderError::UnsupportedAlphaMode)?;
        let alpha_mode = match choice.mode {
            AlphaKind::PreMultiplied => wgpu::CompositeAlphaMode::PreMultiplied,
            // resolve_alpha only yields Opaque when the surface offers it.
            _ => wgpu::CompositeAlphaMode::Opaque,
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.0.max(1),
            height: size.1.max(1),
            desired_maximum_frame_latency: FRAME_LATENCY,
            present_mode,
            alpha_mode,
            view_formats: Vec::new(),
        };
        surface.configure(&gpu.device, &config);
        let info = PresentInfo {
            present_mode: present_kind(present_mode),
            alpha: choice.mode,
            opaque_fallback: choice.opaque_fallback,
            format,
            adapter: gpu.report().clone(),
        };
        Ok(Self {
            gpu,
            target,
            surface,
            config,
            info,
            size,
            needs_reconfigure: false,
            pre_present: None,
        })
    }

    /// The device, for building the [`Renderer`].
    #[must_use]
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Installs the hook that sends the Wayland requests that must precede the commit
    /// (ack_configure, geometry, regions, `wl_surface.frame`, `wp_presentation.feedback`, in
    /// that order). Runs on the render thread right before `present`.
    pub fn set_pre_present(&mut self, hook: Box<dyn FnMut() + Send>) {
        self.pre_present = Some(hook);
    }

    fn configure(&mut self) {
        self.config.width = self.size.0.max(1);
        self.config.height = self.size.1.max(1);
        self.surface.configure(&self.gpu.device, &self.config);
        self.needs_reconfigure = false;
    }

    fn recreate_surface(&mut self) -> bool {
        match self.gpu.instance.create_surface(Arc::clone(&self.target)) {
            Ok(s) => {
                self.surface = s;
                self.configure();
                true
            }
            Err(_) => false,
        }
    }

    fn skip(renderer: &mut Renderer, fault: SurfaceFault) -> PresentOutcome {
        renderer.invalidate_all();
        PresentOutcome::Skipped(fault)
    }
}

impl Presenter for SurfacePresenter {
    fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn resize(&mut self, size: (u32, u32)) {
        if size != self.size {
            self.size = size;
            self.needs_reconfigure = true;
        }
    }

    fn info(&self) -> PresentInfo {
        self.info.clone()
    }

    fn present(&mut self, renderer: &mut Renderer, input: &FrameInput<'_>) -> PresentOutcome {
        if self.gpu.is_lost() {
            renderer.invalidate_all();
            return PresentOutcome::DeviceLost;
        }
        if self.needs_reconfigure {
            self.configure();
        }
        // RENDERING-68. Outdated and Suboptimal reconfigure, Lost recreates the surface,
        // Occluded and Timeout skip the frame.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                self.needs_reconfigure = true;
                t
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configure();
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    _ => return Self::skip(renderer, SurfaceFault::Outdated),
                }
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.recreate_surface();
                return Self::skip(renderer, SurfaceFault::Lost);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return Self::skip(renderer, SurfaceFault::Occluded);
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                return Self::skip(renderer, SurfaceFault::Timeout);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Self::skip(renderer, SurfaceFault::Fatal);
            }
        };
        let t0 = Instant::now();
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let draw = renderer.draw(&mut enc, &view, self.size, input);
        self.gpu.queue.submit(std::iter::once(enc.finish()));
        let record = t0.elapsed();
        let t1 = Instant::now();
        let queue = &self.gpu.queue;
        hand_off(
            self.pre_present
                .as_mut()
                .map(|h| h.as_mut() as &mut dyn FnMut()),
            || queue.present(frame),
        );
        let returned_at = Instant::now();
        PresentOutcome::Presented(FrameStats {
            draw,
            record,
            present_call: returned_at.duration_since(t1),
            returned_at,
        })
    }
}

/// Commit ownership (T-07 §3.1): the pre-present hook (ack, geometry, frame request, feedback)
/// runs strictly before the present that hands the buffer to the compositor, and nothing
/// commits after it.
fn hand_off(pre_present: Option<&mut dyn FnMut()>, present: impl FnOnce()) {
    if let Some(hook) = pre_present {
        hook();
    }
    present();
}

/// True for adapters that are software rasterisers (for `apoth doctor`).
#[must_use]
pub fn is_software(info: &PresentInfo) -> bool {
    info.adapter.class == AdapterClass::Cpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_map_to_policy_kinds() {
        assert_eq!(
            present_kind(wgpu::PresentMode::Mailbox),
            PresentKind::Mailbox
        );
        assert_eq!(present_kind(wgpu::PresentMode::Fifo), PresentKind::Fifo);
        assert_eq!(
            present_kind(wgpu::PresentMode::Immediate),
            PresentKind::Other
        );
        assert_eq!(
            alpha_kind(wgpu::CompositeAlphaMode::PreMultiplied),
            AlphaKind::PreMultiplied
        );
        assert_eq!(
            alpha_kind(wgpu::CompositeAlphaMode::Inherit),
            AlphaKind::Other
        );
    }

    #[test]
    fn commit_ownership_hook_runs_before_present_and_nothing_after() {
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let l = Arc::clone(&log);
        let mut hook: Box<dyn FnMut() + Send> = Box::new(move || l.lock().unwrap().push("hook"));
        let l = Arc::clone(&log);
        hand_off(Some(&mut *hook as &mut dyn FnMut()), || {
            l.lock().unwrap().push("present")
        });
        assert_eq!(*log.lock().unwrap(), ["hook", "present"]);
        // Without a hook, present still runs exactly once.
        let l = Arc::clone(&log);
        hand_off(None, || l.lock().unwrap().push("present"));
        assert_eq!(log.lock().unwrap().len(), 3);
    }

    #[test]
    fn immediate_is_never_requested() {
        let k = choose_present_mode(&[present_kind(wgpu::PresentMode::Immediate)]);
        assert_eq!(k, PresentKind::Fifo);
    }
}
