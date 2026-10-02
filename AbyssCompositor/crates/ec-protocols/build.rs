// SPDX-License-Identifier: Apache-2.0
//! wayland-scanner reads the XML at macro time, which cargo does not track.

fn main() {
    println!("cargo:rerun-if-changed=protocols/eclipse-agent-v1.xml");
}
