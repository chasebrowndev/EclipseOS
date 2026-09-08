// SPDX-License-Identifier: AGPL-3.0-only
//! wlr-layer-shell: panels, bars, backgrounds and lock-adjacent surfaces
//! (COMP-06 §3). Layer surfaces are ordinary clients; they get no privilege
//! beyond their layer, and the trusted UI is never one of them.

use smithay::{
    delegate_layer_shell,
    desktop::{layer_map_for_output, LayerSurface as DesktopLayerSurface, PopupKind, WindowSurfaceType},
    reexports::wayland_server::protocol::wl_output::WlOutput,
    wayland::shell::{
        wlr_layer::{Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState},
        xdg::PopupSurface,
    },
};

use crate::{shell, state::AbyssState};

impl WlrLayerShellHandler for AbyssState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        wl_output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(smithay::output::Output::from_resource)
            .or_else(|| shell::focused_output(self));
        let Some(output) = output else {
            tracing::warn!(%namespace, "layer surface with no output; closing");
            surface.send_close();
            return;
        };
        tracing::info!(%namespace, ?layer, "layer surface mapped");
        let desktop = DesktopLayerSurface::new(surface, namespace);
        {
            let mut map = layer_map_for_output(&output);
            if let Err(err) = map.map_layer(&desktop) {
                tracing::warn!(?err, "mapping layer surface");
                return;
            }
        }
        shell::arrange(self);
    }

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!(?err, "tracking layer-shell popup");
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let Some(output) = self.outputs.iter().map(|e| e.output.clone()).find(|o| {
            layer_map_for_output(o)
                .layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL)
                .is_some()
        }) else {
            return;
        };
        let mut refocus = false;
        {
            let mut map = layer_map_for_output(&output);
            if let Some(layer) = map
                .layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL)
                .cloned()
            {
                refocus = layer.can_receive_keyboard_focus();
                map.unmap_layer(&layer);
            }
        }
        shell::arrange(self);
        if refocus {
            shell::refocus_topmost(self);
        }
    }
}

delegate_layer_shell!(AbyssState);
