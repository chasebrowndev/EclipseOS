// SPDX-License-Identifier: AGPL-3.0-only
//! wayland-scanner reads the XML at macro time, which cargo does not track.

fn main() {
    println!("cargo:rerun-if-changed=protocols/eclipse-protected-surface-v1.xml");
}
