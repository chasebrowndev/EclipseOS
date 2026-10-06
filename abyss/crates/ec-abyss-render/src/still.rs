// SPDX-License-Identifier: AGPL-3.0-only
//! Effects for a still frame: the capture pass (CAP-01, COMP-02 §8, §9).
//!
//! `capture.rs` builds its own, already-redacted pass list and hands each
//! surviving window and layer here, so a screenshot shows what the human sees:
//! opacity, dim-inactive, rounding, drop shadow, glow and blurred backdrops.
//! Nothing here decides what reaches the target; that stays in `capture.rs`.
//!
//! Two properties hold by construction:
//!
//! - **A backdrop samples only the capture list.** [`Still`] owns its own
//!   effect store, blur chain included, separate from the on-screen one. A
//!   backdrop is built from the elements behind it in the list it is spliced
//!   into, which is the redacted list: a blurred secret is a blurred
//!   placeholder. The on-screen backdrops, which did sample the secret, are
//!   never read.
//! - **The frame is settled.** Every window is drawn at its placed geometry
//!   and its final focus state. This module never reads the on-screen
//!   store's time-varying state (a scan test pins that), so nothing in flight
//!   reaches a capture.
//!
//! Borders are left out, as before: they are the compositor's chrome.

use smithay::{
    backend::renderer::{
        element::{
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::CropRenderElement,
            Kind,
        },
        gles::GlesRenderer,
    },
    desktop::{LayerSurface, PopupManager, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale},
};

use ec_abyss_config::{BlurMode, Config};

use crate::{
    blur, effects, glow_colors, glow_program, insert_blur, is_scrim, layer_radius, layer_shape, layer_shows,
    opaque_of, phys, push_dim, push_glow, push_popups, push_shadow, shadow_program, userdata,
    window_backdrop, AbyssRenderElement, BlurRequest, BorderStore,
};

/// The capture pass's own effect store. Kept between captures so a
/// screencast does not recompile shaders or rebuild its blur chain per frame.
#[derive(Default)]
pub struct Still {
    store: BorderStore,
}

/// One capture's effect state for one output. Feed it windows and layers in
/// the order they are pushed (front to back), then [`Frame::finish`].
pub struct Frame<'a> {
    store: &'a mut BorderStore,
    output: &'a Output,
    config: &'a Config,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    rounding: Option<((i32, bool), smithay::backend::renderer::gles::GlesTexProgram)>,
    shadow: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    glow: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    requests: Vec<BlurRequest>,
}

impl Still {
    /// Start a capture of `output` at `output_geo` (global logical).
    /// `windows` are the windows the capture will draw, which bounds what
    /// the store keeps.
    pub fn begin<'a>(
        &'a mut self,
        renderer: &mut GlesRenderer,
        output: &'a Output,
        output_geo: Rectangle<i32, Logical>,
        config: &'a Config,
        windows: &[Window],
    ) -> Frame<'a> {
        let store = &mut self.store;
        let deco = &config.decoration;
        if deco.dim_inactive > 0.0 {
            store.dims.retain(|(o, w), _| o != output || windows.contains(w));
        } else {
            store.dims.clear();
        }
        let fb_height = effects::fb_y_mirrored(output.current_transform())
            .zip(output.current_mode())
            .filter(|_| deco.rounding > 0)
            .map(|(mirrored, mode)| (mode.size.h, mirrored));
        if fb_height.is_some() && store.rounded.is_none() {
            match effects::compile_rounded(renderer) {
                Ok(program) => store.rounded = Some(program),
                Err(err) => tracing::warn!(?err, "compiling the rounded-corner shader; capture unrounded"),
            }
        }
        let rounding = fb_height.zip(store.rounded.clone());
        let shadow = shadow_program(renderer, store, output, config, windows);
        let glow = glow_program(renderer, store, output, config, windows);
        Frame {
            store,
            output,
            config,
            output_geo,
            scale: Scale::from(output.current_scale().fractional_scale()),
            rounding,
            shadow,
            glow,
            requests: Vec::new(),
        }
    }
}

impl Frame<'_> {
    /// Push one window, placed at `geo` (global logical, window geometry),
    /// with its effects: dim overlay, popups, surfaces, glow, shadow, and a
    /// backdrop request spliced in by [`Frame::finish`].
    pub fn window(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        geo: Rectangle<i32, Logical>,
        active: bool,
        out: &mut Vec<AbyssRenderElement>,
    ) {
        let (scale, deco, output_loc) = (self.scale, &self.config.decoration, self.output_geo.loc);
        let alpha = userdata::opacity_of(window).unwrap_or(if active {
            deco.active_opacity
        } else {
            deco.inactive_opacity
        });
        if !active && deco.dim_inactive > 0.0 {
            push_dim(
                self.store,
                self.output,
                window,
                geo,
                output_loc,
                scale,
                deco.dim_inactive * alpha,
                out,
            );
        }

        // Popups apart from the window's own tree, so the rounding mask that
        // follows the window never cuts a menu.
        let render_loc = phys(geo.loc - window.geometry().loc - output_loc, scale);
        let (popups, surfaces) = match window.toplevel() {
            Some(toplevel) => {
                let surface = toplevel.wl_surface();
                let popups = PopupManager::popups_for_surface(surface)
                    .flat_map(|(popup, popup_offset)| {
                        let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
                            .to_physical_precise_round(scale);
                        render_elements_from_surface_tree(
                            renderer,
                            popup.wl_surface(),
                            render_loc + offset,
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    })
                    .collect();
                let own = render_elements_from_surface_tree(
                    renderer,
                    surface,
                    render_loc,
                    scale,
                    alpha,
                    Kind::Unspecified,
                );
                (popups, own)
            }
            None => (
                Vec::new(),
                smithay::backend::renderer::element::AsRenderElements::render_elements::<
                    WaylandSurfaceRenderElement<GlesRenderer>,
                >(window, renderer, render_loc, scale, alpha),
            ),
        };

        let rect: Rectangle<i32, Physical> = Rectangle::new(
            phys(geo.loc - output_loc, scale),
            geo.size.to_f64().to_physical(scale).to_i32_round(),
        );
        let global = deco.blur.mode;
        let mode = userdata::blur_of(window).map_or(global, |rule| rule.resolve(global));
        let corner = match self.rounding {
            Some(_) => (deco.rounding as f64 * scale.x.max(scale.y)).ceil() as i32,
            None => 0,
        };
        // No bezel: it replaces the border, and borders are not captured.
        let backdrop = (mode != BlurMode::Off)
            .then(|| {
                window_backdrop(
                    mode,
                    alpha,
                    rect,
                    corner,
                    opaque_of(&surfaces, scale),
                    deco.rounding,
                    self.config.general.border_size,
                    None,
                )
            })
            .flatten();

        push_popups(out, popups, scale, None);
        match &self.rounding {
            Some(((fb_height, mirrored), program)) => {
                let radius = deco.rounding as f64 * scale.x.max(scale.y);
                let uniforms = effects::rounding_uniforms(rect, *fb_height, *mirrored, radius as f32);
                out.extend(surfaces.into_iter().map(|s| {
                    AbyssRenderElement::Rounded(effects::RoundedElement::new(
                        s,
                        program.clone(),
                        uniforms.clone(),
                    ))
                }));
            }
            None => out.extend(surfaces.into_iter().map(AbyssRenderElement::Surface)),
        }

        if let Some(program) = &self.glow {
            let (on, off) = glow_colors(self.config);
            let color = if active { on } else { off };
            push_glow(
                self.store,
                program,
                self.output,
                window,
                geo,
                output_loc,
                self.config,
                color,
                out,
            );
        }
        if let Some(program) = &self.shadow {
            let focus = if active { 1.0 } else { 0.0 };
            push_shadow(
                self.store,
                program,
                self.output,
                window,
                geo,
                output_loc,
                self.config,
                focus,
                out,
            );
        }

        if let Some((region, radius, _)) = backdrop {
            self.requests.push((
                blur::BlurKey::Window(window.clone()),
                out.len(),
                region,
                mode,
                radius,
                None,
                None,
                None,
            ));
        }
    }

    /// Push one layer surface at `geo` (output-local logical), with its
    /// backdrop when its glass shows. Layers take the global blur mode.
    pub fn layer(
        &mut self,
        renderer: &mut GlesRenderer,
        surface: &LayerSurface,
        geo: Rectangle<i32, Logical>,
        out: &mut Vec<AbyssRenderElement>,
    ) {
        let scale = self.scale;
        let loc = phys(geo.loc, scale);
        let els = smithay::backend::renderer::element::AsRenderElements::render_elements::<
            WaylandSurfaceRenderElement<GlesRenderer>,
        >(surface, renderer, loc, scale, 1.0);
        let mode = self.config.decoration.blur.mode;
        let out_rect = Rectangle::new(
            Point::from((0, 0)),
            self.output_geo.size.to_f64().to_physical(scale).to_i32_round(),
        );
        let region = Rectangle::new(loc, geo.size.to_f64().to_physical(scale).to_i32_round());
        let crop = (!out_rect.contains_rect(region)).then_some(out_rect);
        let wants = mode != BlurMode::Off && !is_scrim(surface, geo.size, self.output_geo.size);
        let shape = wants
            .then(|| layer_shape(surface, geo.size, loc, scale, self.config))
            .flatten();
        let shows = wants && layer_shows(region, shape.as_ref(), opaque_of(&els, scale));
        let els: Vec<AbyssRenderElement> = match crop {
            Some(crop) => els
                .into_iter()
                .filter_map(|e| CropRenderElement::from_element(e, scale, crop))
                .map(AbyssRenderElement::Cropped)
                .collect(),
            None => els.into_iter().map(AbyssRenderElement::Surface).collect(),
        };
        if shows {
            self.requests.push((
                blur::BlurKey::Layer(surface.clone()),
                out.len() + els.len(),
                region,
                mode,
                layer_radius(surface, self.config),
                None,
                shape,
                crop,
            ));
        }
        out.extend(els);
    }

    /// Splice in every backdrop requested, each sampling only what lies
    /// behind it in `out`.
    pub fn finish(self, renderer: &mut GlesRenderer, out: &mut Vec<AbyssRenderElement>) {
        insert_blur(renderer, self.output, self.store, self.config, self.requests, out);
    }
}
