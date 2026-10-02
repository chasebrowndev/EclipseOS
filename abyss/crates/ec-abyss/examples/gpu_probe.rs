// SPDX-License-Identifier: AGPL-3.0-only
//! Render-node GPU probe (COMP-01 §3).
//!
//! Walks the EGL/GBM half of the DRM backend's startup sequence
//! (`backend/drm.rs`, the `GbmDevice::new` → `EGLDisplay::new` →
//! `EGLContext::new` → `GlesRenderer::new` run) against a *render node*, which
//! needs neither DRM master nor a free VT nor a seat. It can therefore be run
//! from inside an ordinary desktop session to answer "does EGL come up on this
//! driver?" without booting the compositor on a second VT.
//!
//! Deliberately absent: `LibSeatSession`, `UdevBackend`, `DrmDevice::new` and
//! anything that modesets. Those need DRM master and belong to the real boot.
//!
//! ```text
//! cargo run --example gpu_probe [/dev/dri/renderD128]
//! ```

use std::{fs::File, os::fd::OwnedFd, path::PathBuf};

use anyhow::{Context, Result};
use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        drm::{DrmDeviceFd, DrmNode, NodeType},
        egl::{context::EGLContext, display::EGLDisplay},
        renderer::gles::GlesRenderer,
    },
    utils::DeviceFd,
    wayland::drm_syncobj::supports_syncobj_eventfd,
};

const DEFAULT_NODE: &str = "/dev/dri/renderD128";

fn main() {
    if let Err(err) = probe() {
        // The whole chain, not just the top line: a `modeset=0` boot surfaces
        // here as an opaque EGL error and the cause is in the tail.
        eprintln!("FAIL: {err:#}");
        for (i, cause) in err.chain().skip(1).enumerate() {
            eprintln!("  cause {}: {cause}", i + 1);
        }
        std::process::exit(1);
    }
}

fn probe() -> Result<()> {
    let path: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_NODE));
    println!("node: {}", path.display());

    let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));

    // --- node identity (drm.rs resolves the same way) ----------------------
    let node = DrmNode::from_file(&device_fd).context("DrmNode::from_file")?;
    println!("  type      : {:?}", node.ty());
    println!("  dev_id    : {}", node.dev_id());
    match node.node_with_type(NodeType::Render) {
        Some(Ok(render)) => println!("  render    : dev_id {}", render.dev_id()),
        Some(Err(err)) => println!("  render    : unresolvable ({err})"),
        None => println!("  render    : none advertised"),
    }

    // --- GBM ---------------------------------------------------------------
    let gbm = GbmDevice::new(device_fd.clone()).context("GbmDevice::new")?;
    println!("gbm: ok");

    // --- EGL: the step this probe exists for -------------------------------
    // SAFETY: the gbm device outlives the display; both live to the end of
    // this function.
    let egl_display = unsafe { EGLDisplay::new(gbm) }.context("EGLDisplay::new")?;
    let (major, minor) = egl_display.get_egl_version();
    println!("egl: ok, version {major}.{minor}");
    println!(
        "  display dmabuf texture formats: {}",
        egl_display.dmabuf_texture_formats().iter().count()
    );
    println!(
        "  display dmabuf render formats : {}",
        egl_display.dmabuf_render_formats().iter().count()
    );
    println!("  extensions ({}):", egl_display.extensions().len());
    for ext in egl_display.extensions() {
        println!("    {ext}");
    }

    let egl_context = EGLContext::new(&egl_display).context("EGLContext::new")?;
    println!("egl context: ok");

    // SAFETY: the context is current on this (single) thread only.
    let renderer = unsafe { GlesRenderer::new(egl_context) }.context("GlesRenderer::new")?;
    println!(
        "gles renderer: ok, {} dmabuf texture formats",
        renderer.egl_context().dmabuf_texture_formats().iter().count()
    );

    // --- explicit sync probe ----------------------------------------------
    // smithay 0.7.0's probe ends in `Ok(_) => unreachable!()`; drm.rs wraps it
    // in `catch_unwind` for exactly that reason. Report which branch a real
    // driver takes.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supports_syncobj_eventfd(&device_fd)
    }));
    match result {
        Ok(true) => println!("syncobj eventfd: supported (explicit sync would be enabled)"),
        Ok(false) => println!("syncobj eventfd: unsupported (no explicit sync global)"),
        Err(_) => println!("syncobj eventfd: PANICKED — catch_unwind in drm.rs is load-bearing here"),
    }

    println!("\nOK");
    Ok(())
}
