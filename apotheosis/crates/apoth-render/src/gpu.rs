// SPDX-License-Identifier: AGPL-3.0-only
//! The wgpu device: instance, adapter selection, device and queue (T-07 §3; RENDERING-30,
//! PERFORMANCE-76).
//!
//! Vulkan only. The adapter is chosen by [`crate::surface::choose_adapter`]: the adapter the
//! compositor renders on if the caller names it, hardware over software whenever hardware
//! offers an alpha-capable surface. If device creation fails the next best adapter is tried
//! (RENDERING-30). A software adapter is used only when `allow_software` is set
//! (`render.allow-software`); otherwise the error names the missing Vulkan driver (T-09 §7).
//!
//! The default wgpu uncaptured-error handler panics on a validation error. That is kept on
//! purpose: a validation error is a renderer bug and the goldens must fail loudly.

use crate::surface::{AdapterCandidate, AdapterClass, choose_adapter};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;

/// Renderer setup failures.
#[derive(Debug)]
pub enum RenderError {
    /// No usable Vulkan adapter. The text names what is missing.
    NoAdapter(String),
    /// Device creation failed on every adapter tried.
    Device(String),
    /// Surface creation or configuration failed.
    Surface(String),
    /// The surface offers no colour format.
    NoSurfaceFormat,
    /// An explicit alpha mode was requested that the surface does not offer (RENDERING-62).
    UnsupportedAlphaMode,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter(s) => write!(f, "no usable Vulkan adapter: {s}"),
            Self::Device(s) => write!(f, "device creation failed: {s}"),
            Self::Surface(s) => write!(f, "surface error: {s}"),
            Self::NoSurfaceFormat => write!(f, "the surface offers no colour format"),
            Self::UnsupportedAlphaMode => {
                write!(f, "the requested composite alpha mode is not supported")
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// How to pick the adapter.
#[derive(Clone, Debug, Default)]
pub struct GpuOptions {
    /// `render.allow-software`: accept a CPU adapter (lavapipe) when nothing else exists.
    pub allow_software: bool,
    /// Names the adapter the compositor renders on: a PCI bus id (`0000:01:00.0`) or a part
    /// of the adapter name. Wgpu has no portable way to ask the compositor, so the caller
    /// (apoth-wl, from the compositor's device) supplies it.
    pub compositor_hint: Option<String>,
}

/// What `apoth doctor` reports about the adapter.
#[derive(Clone, Debug)]
pub struct AdapterReport {
    /// Adapter name.
    pub name: String,
    /// Adapter class.
    pub class: AdapterClass,
    /// Driver name and info.
    pub driver: String,
}

/// Instance, adapter, device and queue. Cheap to clone: every field is a handle.
#[derive(Clone, Debug)]
pub struct Gpu {
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    report: AdapterReport,
    lost: Arc<AtomicBool>,
}

pub(crate) fn class_of(t: wgpu::DeviceType) -> AdapterClass {
    match t {
        wgpu::DeviceType::DiscreteGpu => AdapterClass::Discrete,
        wgpu::DeviceType::IntegratedGpu => AdapterClass::Integrated,
        wgpu::DeviceType::VirtualGpu => AdapterClass::Virtual,
        wgpu::DeviceType::Cpu => AdapterClass::Cpu,
        wgpu::DeviceType::Other => AdapterClass::Other,
    }
}

/// The process's Vulkan-only instance (no GL, no other backend is compiled in anyway). One
/// for every window and offscreen device, created once and never destroyed: a second
/// window's device must not take the instance down under the first (the NVIDIA driver
/// faults in the surviving window's swapchain when a sibling instance is destroyed).
pub(crate) fn new_instance() -> wgpu::Instance {
    static INSTANCE: std::sync::OnceLock<wgpu::Instance> = std::sync::OnceLock::new();
    INSTANCE
        .get_or_init(|| {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = wgpu::Backends::VULKAN;
            wgpu::Instance::new(desc)
        })
        .clone()
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Drives a future to completion on the calling thread. Used only for device setup, where
/// wgpu's native futures are ready at once; it is not used on a frame path.
pub(crate) fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}

const MISSING_DRIVER: &str = "no Vulkan adapter found; install the Vulkan driver for this GPU \
     (vulkan-radeon, vulkan-intel or nvidia-utils); software rendering needs lavapipe \
     (vulkan-swrast) and render.allow-software";

impl Gpu {
    /// A device with no surface (offscreen rendering, goldens).
    pub fn headless(opts: &GpuOptions) -> Result<Self, RenderError> {
        Self::from_instance(new_instance(), opts, None)
    }

    /// Picks an adapter on `instance` (restricted to those that can present to `surface` when
    /// given) and opens a device on it.
    pub(crate) fn from_instance(
        instance: wgpu::Instance,
        opts: &GpuOptions,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, RenderError> {
        let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let mut cands: Vec<(wgpu::Adapter, AdapterCandidate)> = Vec::new();
        for adapter in adapters {
            let info = adapter.get_info();
            let alpha_capable = match surface {
                Some(s) => {
                    if !adapter.is_surface_supported(s) {
                        continue;
                    }
                    s.get_capabilities(&adapter)
                        .alpha_modes
                        .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
                }
                None => true,
            };
            let compositor = opts.compositor_hint.as_deref().is_some_and(|h| {
                !h.is_empty() && (info.device_pci_bus_id == h || info.name.contains(h))
            });
            let cand = AdapterCandidate {
                class: class_of(info.device_type),
                alpha_capable,
                compositor,
            };
            cands.push((adapter, cand));
        }

        let mut problems: Vec<String> = Vec::new();
        while !cands.is_empty() {
            let metas: Vec<AdapterCandidate> = cands.iter().map(|c| c.1).collect();
            let Some(i) = choose_adapter(&metas) else {
                break;
            };
            let (adapter, cand) = cands.remove(i);
            let info = adapter.get_info();
            if cand.class == AdapterClass::Cpu && !opts.allow_software {
                problems.push(format!(
                    "software adapter \"{}\" rejected (render.allow-software is off)",
                    info.name
                ));
                continue;
            }
            let desc = wgpu::DeviceDescriptor {
                label: Some("apoth-render"),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            };
            match block_on(adapter.request_device(&desc)) {
                Ok((device, queue)) => {
                    let lost = Arc::new(AtomicBool::new(false));
                    let flag = Arc::clone(&lost);
                    device.set_device_lost_callback(move |_reason, _message| {
                        flag.store(true, Ordering::SeqCst);
                    });
                    let report = AdapterReport {
                        name: info.name.clone(),
                        class: cand.class,
                        driver: format!("{} {}", info.driver, info.driver_info),
                    };
                    return Ok(Self {
                        instance,
                        adapter,
                        device,
                        queue,
                        report,
                        lost,
                    });
                }
                Err(e) => problems.push(format!("device on \"{}\": {e:?}", info.name)),
            }
        }
        if problems.is_empty() {
            Err(RenderError::NoAdapter(MISSING_DRIVER.to_owned()))
        } else {
            Err(RenderError::NoAdapter(problems.join("; ")))
        }
    }

    /// The device was lost: rebuild device, pipelines and atlases from CPU state
    /// (PERFORMANCE-76). The scene and glyph caches live on the CPU, so a new [`Gpu`], a new
    /// [`crate::renderer::Renderer`] and a cleared atlas provider are the whole recovery.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// The adapter in use, for `apoth doctor`.
    #[must_use]
    pub fn report(&self) -> &AdapterReport {
        &self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_runs_a_ready_future() {
        assert_eq!(block_on(async { 7 }), 7);
    }

    #[test]
    fn device_types_map_to_classes() {
        assert_eq!(class_of(wgpu::DeviceType::Cpu), AdapterClass::Cpu);
        assert_eq!(
            class_of(wgpu::DeviceType::DiscreteGpu),
            AdapterClass::Discrete
        );
    }

    #[test]
    fn errors_name_what_is_missing() {
        let e = RenderError::NoAdapter(MISSING_DRIVER.to_owned());
        assert!(e.to_string().contains("Vulkan driver"));
    }
}
