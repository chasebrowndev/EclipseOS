// SPDX-License-Identifier: AGPL-3.0-only
//! Compile every backdrop program (rounded, frost, glass, their shaped
//! variants, the annotation HUD glass) on the local GPU through a surfaceless
//! EGL display, and print the driver's info log for any that fails. Dev tool,
//! not shipped; exits 1 on a failure, 0 when all compile or no GPU exists.

use smithay::backend::egl::context::EGLContext;
use smithay::backend::egl::native::EGLSurfacelessDisplay;
use smithay::backend::egl::{EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::{GlesError, GlesRenderer, GlesTexProgram};

use ec_abyss_render::effects;

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

type Compile = fn(&mut GlesRenderer) -> Result<GlesTexProgram, GlesError>;

fn main() {
    // smithay logs the driver's info log through `tracing::error!`.
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let mut renderer = match renderer() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("no GPU renderer ({e}); skipping");
            return;
        }
    };
    let programs: [(&str, Compile); 7] = [
        ("rounded", effects::compile_rounded),
        ("rounded-shaped", effects::compile_rounded_shaped),
        ("frost", effects::compile_frost),
        ("frost-shaped", effects::compile_frost_shaped),
        ("glass", effects::compile_glass),
        ("glass-shaped", effects::compile_glass_shaped),
        ("hud-glass", effects::compile_hud_glass),
    ];
    let mut failed = false;
    for (name, compile) in programs {
        match compile(&mut renderer) {
            Ok(_) => println!("ok    {name}"),
            Err(e) => {
                failed = true;
                println!("FAIL  {name}: {e}");
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
