// SPDX-License-Identifier: AGPL-3.0-only
//! Connector-property probe: does this driver expose overscan compensation?
//!
//! Enumerates every connector on a card node and prints each of its DRM
//! properties by name, type and current value, calling out the three the
//! overscan question turns on: `underscan`, `underscan hborder` and
//! `underscan vborder`.
//!
//! Reading connector properties needs a DRM fd but *not* DRM master, and
//! `chase` is in the `video` group, so this runs inside a live desktop session
//! at zero console cost — the same trick `gpu_probe.rs` uses to avoid needing a
//! free VT. Deliberately absent: anything that commits, modesets or takes
//! master.
//!
//! Why it matters: if the driver exposes the underscan properties, overscan
//! compensation is "set two properties on commit" and the logical-geometry
//! inset system (DP-1) never has to be built. If it does not, the fallback is
//! to scale the whole scene down into an inset rectangle and leave the margins
//! black — nothing is cropped, every pixel is still on screen.
//!
//! ```text
//! cargo run --example underscan_probe [/dev/dri/card1]
//! ```
//!
//! Note the default node is `card1`. There is no `card0` on this machine.

use std::{fs::File, os::fd::OwnedFd, path::PathBuf};

use anyhow::{Context, Result};
use smithay::{
    backend::drm::DrmDeviceFd,
    reexports::drm::control::{property, Device as ControlDevice, ModeTypeFlags},
    utils::DeviceFd,
};

const DEFAULT_NODE: &str = "/dev/dri/card1";

/// The properties this probe exists to answer for. Matched case-insensitively
/// because the kernel spells them with spaces and drivers have been known to
/// differ on case.
const WANTED: [&str; 3] = ["underscan", "underscan hborder", "underscan vborder"];

fn main() {
    if let Err(err) = probe() {
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
    let device = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));

    let resources = device.resource_handles().context("resource_handles")?;
    println!("connectors: {}", resources.connectors().len());

    // Accumulated across every connector so the verdict does not depend on
    // reading the whole dump.
    let mut found_any = false;

    for handle in resources.connectors() {
        // `false`: do not force a probe. A probe needs no master but does cost
        // a modeset-path round trip on some drivers, and the connector state
        // cached from the running session is what we want anyway.
        let info = match device.get_connector(*handle, false) {
            Ok(info) => info,
            Err(err) => {
                println!("\n=== connector {handle:?}: unreadable ({err}) ===");
                continue;
            }
        };

        println!(
            "\n=== {}-{} ({:?}) ===",
            info.interface().as_str(),
            info.interface_id(),
            info.state()
        );
        if let Some((w, h)) = info.size() {
            println!("  physical size: {w}mm x {h}mm");
        }

        print_modes(&info);
        found_any |= print_properties(&device, *handle)?;
    }

    println!("\n---");
    if found_any {
        println!("VERDICT: underscan properties ARE exposed on at least one connector.");
        println!("  Overscan compensation can be done in the kernel: set the properties on");
        println!("  commit. The DP-1 logical-geometry inset system is not needed.");
    } else {
        println!("VERDICT: NO underscan properties on any connector.");
        println!("  Kernel-side overscan compensation is unavailable on this driver.");
        println!("  The fallback is compositor-side scale-and-pad (see DP-1): the scene is");
        println!("  scaled into an inset rect and the margins left black. Nothing is cropped.");
    }

    Ok(())
}

/// Mode list with the type flags spelled out. Included because more than one
/// mode reporting `PREFERRED` is an open defect against mode selection, and
/// this probe is already holding the connector.
fn print_modes(info: &smithay::reexports::drm::control::connector::Info) {
    let modes = info.modes();
    println!("  modes: {}", modes.len());
    let preferred = modes
        .iter()
        .filter(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .count();
    for mode in modes.iter().take(12) {
        let (w, h) = mode.size();
        println!(
            "    {:>5}x{:<5} {:>4} Hz   {:?}",
            w,
            h,
            mode.vrefresh(),
            mode.mode_type()
        );
    }
    if modes.len() > 12 {
        println!("    … {} more", modes.len() - 12);
    }
    if preferred > 1 {
        println!("  NOTE: {preferred} modes report PREFERRED — mode selection is ambiguous here.");
    }
}

/// Prints every property on the connector. Returns whether any of [`WANTED`]
/// was present.
fn print_properties(
    device: &DrmDeviceFd,
    handle: smithay::reexports::drm::control::connector::Handle,
) -> Result<bool> {
    let props = device
        .get_properties(handle)
        .context("get_properties(connector)")?;

    println!("  properties: {}", props.as_props_and_values().0.len());
    let mut found = false;

    for (prop, raw) in props.iter() {
        let info = match device.get_property(*prop) {
            Ok(info) => info,
            Err(err) => {
                println!("    <unreadable property {prop:?}: {err}>");
                continue;
            }
        };
        let name = info.name().to_string_lossy().into_owned();
        let wanted = WANTED.iter().any(|w| w.eq_ignore_ascii_case(&name));
        found |= wanted;

        let marker = if wanted { ">>" } else { "  " };
        let flags = match (info.mutable(), info.atomic()) {
            (true, true) => "mutable,atomic",
            (true, false) => "mutable",
            (false, true) => "immutable,atomic",
            (false, false) => "immutable",
        };
        println!(
            "  {marker} {name:<28} = {:<24} [{flags}]",
            format_value(&info, *raw)
        );

        // For an enum, the set of accepted values is the actionable part: it
        // says what can be written, not just what is set now.
        if wanted {
            if let property::ValueType::Enum(values) = info.value_type() {
                let (raws, enums) = values.values();
                for (v, e) in raws.iter().zip(enums.iter()) {
                    println!("        option {v} = {}", e.name().to_string_lossy());
                }
            }
            if let property::ValueType::UnsignedRange(lo, hi) = info.value_type() {
                println!("        range {lo}..={hi}");
            }
        }
    }

    Ok(found)
}

fn format_value(info: &property::Info, raw: property::RawValue) -> String {
    match info.value_type().convert_value(raw) {
        property::Value::Enum(Some(e)) => {
            format!("{} ({raw})", e.name().to_string_lossy())
        }
        property::Value::Enum(None) => format!("<not in enum> ({raw})"),
        property::Value::Boolean(b) => format!("{b}"),
        property::Value::UnsignedRange(v) => format!("{v}"),
        property::Value::SignedRange(v) => format!("{v}"),
        property::Value::Bitmask(v) => format!("bitmask {v:#x}"),
        property::Value::Blob(v) => format!("blob {v}"),
        other => format!("{other:?}"),
    }
}
