// SPDX-License-Identifier: AGPL-3.0-only
//! Effects for a still frame: the capture pass (CAP-01, COMP-02 §8, §9).
//!
//! `capture.rs` builds its own, already-redacted pass list and hands each
//! surviving window and layer here, so a screenshot shows what the human sees:
//! opacity, dim-inactive, rounding, drop shadow, glow, the window border (or
//! its glass bezel), blurred backdrops and `eclipse_glass_v1` panes (GLASS-01).
//! Nothing here decides what reaches the target; that stays in `capture.rs`.
//!
//! Three properties hold by construction:
//!
//! - **A backdrop or pane samples only the capture list.** [`Still`] owns its
//!   own effect store, blur chain included, separate from the on-screen one. A
//!   backdrop or pane is built from the elements behind it in the list it is
//!   spliced into, which is the redacted list: a blurred secret is a blurred
//!   placeholder. The on-screen backdrops, which did sample the secret, are
//!   never read.
//! - **The frame is settled.** Every window is drawn at its placed geometry
//!   and its final focus state; a border or bezel takes the colour its focus
//!   rests at, with no crossfade. This module never reads the on-screen
//!   store's time-varying state (a scan test pins that), so nothing in flight
//!   reaches a capture.
//! - **Panes and the bezel follow the live path.** The same splice rules
//!   (`PaneRequest::before`) and the same bezel decision as `collect_elements`,
//!   so a still matches the screen it was taken from.
//!
//! Trusted UI stays out, as before: `capture.rs` never passes it here.

use smithay::{
    backend::renderer::{
        element::{
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::CropRenderElement,
            Kind,
        },
        gles::GlesRenderer,
        utils::RendererSurfaceStateUserData,
    },
    desktop::{LayerSurface, PopupManager, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{compositor::with_states, seat::WaylandFocus},
};

use ec_abyss_config::{BlurMode, Config};

use crate::{
    bar_depth, bezel_ready, bezel_width, blur, border_frame, committed_panes, effects, ensure_glass,
    explicit_bezel, explicit_blur, explicit_window_backdrop, fills_area, glow_colors, glow_program,
    insert_blur, is_scrim, layer_radius, layer_shape, layer_shows, opaque_of, phys, push_border, push_dim,
    push_glow, push_popups, push_shadow, shadow_program, strip_backdrop, strip_crops, strip_rect,
    union_with_bar, userdata, window_backdrop, window_takes_panes, AbyssRenderElement, Bezel, BlurRequest,
    BorderFrame, BorderStore, PaneRequest, BEZEL_DIM,
};

/// The capture pass's own effect store. Kept between captures so a
/// screencast does not recompile shaders or rebuild its blur chain per frame.
#[derive(Default)]
pub struct Still {
    store: BorderStore,
}

impl Still {
    /// See [`BorderStore::retain_contexts`].
    pub fn retain_contexts(
        &mut self,
        keep: &[smithay::backend::renderer::ContextId<smithay::backend::renderer::gles::GlesTexture>],
    ) {
        self.store.retain_contexts(keep);
    }
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
    /// This output's painted-border frame; `None` when `border-size` is off.
    border: Option<BorderFrame>,
    /// Whether the glass bezel program draws, per backdrop mode (glass, frost),
    /// compiled on first use as the live path does.
    bezel_ok: [Option<bool>; 2],
    requests: Vec<BlurRequest>,
    panes: Vec<PaneRequest>,
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
        // The capture pass draws with whichever renderer it is handed (ADR
        // 0084); its programs follow that renderer.
        store.bind(renderer);
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
        if fb_height.is_some() && store.gpu.rounded.is_none() {
            match effects::compile_rounded(renderer) {
                Ok(program) => store.gpu.rounded = Some(program),
                Err(err) => tracing::warn!(?err, "compiling the rounded-corner shader; capture unrounded"),
            }
        }
        let rounding = fb_height.zip(store.gpu.rounded.clone());
        let scale = Scale::from(output.current_scale().fractional_scale());
        let border = border_frame(renderer, store, output_geo.loc, scale, output, config, windows);
        let shadow = shadow_program(renderer, store, output, config, windows);
        let glow = glow_program(renderer, store, output, config, windows);
        Frame {
            store,
            output,
            config,
            output_geo,
            scale,
            rounding,
            shadow,
            glow,
            border,
            bezel_ok: [None; 2],
            requests: Vec::new(),
            panes: Vec::new(),
        }
    }
}

/// `els` as the capture list takes them: cut to `crop` when the surface spills
/// past the output, as a layer's surfaces are.
fn cropped(
    els: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    scale: Scale<f64>,
    crop: Option<Rectangle<i32, Physical>>,
) -> Vec<AbyssRenderElement> {
    match crop {
        Some(crop) => els
            .into_iter()
            .filter_map(|e| CropRenderElement::from_element(e, scale, crop))
            .map(AbyssRenderElement::Cropped)
            .collect(),
        None => els.into_iter().map(AbyssRenderElement::Surface).collect(),
    }
}

impl Frame<'_> {
    /// Push one window, placed at `geo` (global logical, window geometry),
    /// with its effects: dim overlay, popups, surfaces, border or bezel, glow,
    /// shadow, panes, and a backdrop request spliced in by [`Frame::finish`].
    pub fn window(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        geo: Rectangle<i32, Logical>,
        active: bool,
        out: &mut Vec<AbyssRenderElement>,
    ) {
        let (scale, deco, output_loc) = (self.scale, &self.config.decoration, self.output_geo.loc);
        // Under `mode "de"` the dim, rounded mask, glow and shadow follow the
        // frame, and the surfaces stay on the content, as in `window_elements`.
        let bar = crate::titlebar::bar_of(window);
        // A window rolled up to its bar has no content here either, so what the
        // human shaded away is not captured. Its surfaces and popups stay out.
        let rolled = bar.is_some_and(|b| b.shaded);
        let geo = if rolled {
            Rectangle::new(geo.loc, (geo.size.w, 0).into())
        } else {
            geo
        };
        let frame = bar.map_or(geo, |b| crate::titlebar::frame_of_content(geo, b.height));
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
                frame,
                output_loc,
                scale,
                deco.dim_inactive * alpha,
                out,
            );
        }

        // Popups apart from the window's own tree, so the rounding mask that
        // follows the window never cuts a menu.
        let render_loc = phys(geo.loc - window.geometry().loc - output_loc, scale);
        // Tint-keyed strip (ADR 0079), drawn as the live path does.
        let strip_def = userdata::strip_of(window).filter(|_| !rolled && window.toplevel().is_some());
        let keyed = match strip_def.as_ref().and_then(|s| s.keyed().copied()) {
            Some(keys) => crate::keyed_target(renderer, self.store, self.output)
                .map(|(program, h, mirrored)| (program, h, mirrored, keys)),
            None => None,
        };
        let strip_def = strip_def.filter(|s| s.keyed().is_none() || keyed.is_some());
        let mut popup_groups: Vec<crate::PopupGroup> = Vec::new();
        let (popups, surfaces) = match window.toplevel() {
            _ if rolled => (Vec::new(), Vec::new()),
            Some(toplevel) => {
                let surface = toplevel.wl_surface();
                let mut popups = Vec::new();
                for (n, (popup, popup_offset)) in PopupManager::popups_for_surface(surface).enumerate() {
                    let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
                        .to_physical_precise_round(scale);
                    let els = render_elements_from_surface_tree(
                        renderer,
                        popup.wl_surface(),
                        render_loc + offset,
                        scale,
                        alpha,
                        Kind::Unspecified,
                    );
                    if keyed.is_some() {
                        let at = geo.loc - output_loc + popup_offset;
                        let prect = Rectangle::new(
                            phys(at, scale),
                            popup.geometry().size.to_f64().to_physical(scale).to_i32_round(),
                        );
                        popup_groups.push((
                            n,
                            prect,
                            crate::popup_blur_radius(renderer, &popup, deco.rounding),
                            els,
                        ));
                    } else {
                        popups.extend(els);
                    }
                }
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
        let frame_rect: Rectangle<i32, Physical> = Rectangle::new(
            phys(frame.loc - output_loc, scale),
            frame.size.to_f64().to_physical(scale).to_i32_round(),
        );
        // `translucent-region`: the strip's copy of the surface tree at the
        // strip's alpha, drawn above the strip's bottom edge (see the live
        // path in `window_elements`).
        let strip = strip_def.zip(window.toplevel()).map(|(s, toplevel)| {
            let copy = match s.fill {
                userdata::StripFill::Fade(a) => render_elements_from_surface_tree(
                    renderer,
                    toplevel.wl_surface(),
                    render_loc,
                    scale,
                    alpha * a,
                    Kind::Unspecified,
                ),
                userdata::StripFill::Keyed(_) => Vec::new(),
            };
            let blur_rect = match s.keyed() {
                Some(k) if k.any_everywhere() => rect,
                _ => strip_rect(rect, s.top, scale),
            };
            (blur_rect, copy)
        });
        let key_region = strip_def.map(|s| strip_rect(rect, s.top, scale));
        let global = deco.blur.mode;
        let mode = userdata::blur_of(window).map_or(global, |rule| rule.resolve(global));
        let corner = match self.rounding {
            Some(_) => (deco.rounding as f64 * scale.x.max(scale.y)).ceil() as i32,
            None => 0,
        };
        // The glass bezel replaces the painted border wherever its program
        // draws, as on the live path. Compiled on first use, per mode.
        let glass_mode = matches!(mode, BlurMode::Glass | BlurMode::Frost);
        let ready = match self.border {
            Some(_) if glass_mode => {
                let slot = (mode == BlurMode::Frost) as usize;
                let (store, output) = (&mut *self.store, self.output);
                *self.bezel_ok[slot].get_or_insert_with(|| bezel_ready(renderer, store, output, mode))
            }
            _ => false,
        };
        let border_size = self.config.general.border_size;
        let bezel = bezel_width(mode, ready, fills_area(window), border_size, scale.x.max(scale.y));
        // A settled frame: the bezel's rim is the focus state's resting colour.
        let rim = if active { [1.0; 4] } else { [BEZEL_DIM; 4] };
        // `ext_background_effect_v1`, read exactly as the live path does.
        let explicit = smithay::wayland::seat::WaylandFocus::wl_surface(window)
            .and_then(|surface| explicit_blur(&surface, window.bbox().size, render_loc, scale));
        // A `translucent-region` rule asks for glass behind the strip, so a
        // toolkit's empty blur region (GTK declares one) does not veto it.
        let explicit = explicit.filter(|a| strip.is_none() || *a != blur::BlurArea::Nothing);
        let backdrop = (mode != BlurMode::Off)
            .then(|| {
                let opaque = opaque_of(&surfaces, scale);
                match explicit {
                    Some(area) if !explicit_bezel(&area, rect, bezel) => {
                        // The strip is translucent whatever the client's
                        // opaque region says.
                        let opaque = match &strip {
                            Some((strip, _)) => Rectangle::subtract_rects_many(opaque, [*strip]),
                            None => opaque,
                        };
                        explicit_window_backdrop(area, alpha, corner, opaque, deco.rounding)
                            .map(|(region, radius, shape)| (region, radius, None, shape))
                    }
                    _ => {
                        let backdrop = |opaque| {
                            window_backdrop(
                                mode,
                                alpha,
                                rect,
                                corner,
                                opaque,
                                deco.rounding,
                                border_size,
                                bezel,
                            )
                        };
                        // A bezel already blurs the whole bordered rect, so the
                        // strip only has to stop counting as opaque there.
                        match &strip {
                            Some((strip, _)) if bezel.is_some() => {
                                backdrop(Rectangle::subtract_rects_many(opaque, [*strip]))
                            }
                            Some((strip, _)) => backdrop(opaque.clone()).or_else(|| {
                                strip_backdrop(*strip, rect, corner)
                                    .map(|region| (region, deco.rounding, None))
                            }),
                            None => backdrop(opaque),
                        }
                        .map(|(region, radius, bezel)| {
                            let bezel = bezel.map(|(inner, opaque)| Bezel {
                                inner,
                                opaque,
                                bar: 0,
                                rim,
                            });
                            (region, radius, bezel, None)
                        })
                    }
                }
            })
            .flatten();
        // The titlebar is glass whatever the client is, as in `window_elements`
        // (a still has no bezel, so the region is simply grown over the bar).
        let backdrop = match bar {
            Some(info) if mode != BlurMode::Off && !rolled => {
                let strip = crate::titlebar::bar_rect(geo, info.height);
                let strip = Rectangle::new(
                    phys(strip.loc - output_loc, scale),
                    strip.size.to_f64().to_physical(scale).to_i32_round(),
                );
                match backdrop {
                    None => strip_backdrop(strip, frame_rect, corner)
                        .map(|region| (region, deco.rounding, None, None)),
                    Some((region, radius, bezel, None)) => {
                        let region = union_with_bar(region, strip);
                        // An opaque bezel is ring-only; the bar stays glass.
                        let bezel = bezel.map(|b| Bezel {
                            bar: bar_depth(region, strip),
                            ..b
                        });
                        Some((region, radius, bezel, None))
                    }
                    other => other,
                }
            }
            _ => backdrop,
        };

        // `eclipse_glass_v1` on a window (GLASS-01): glass under each pane,
        // directly above the backdrop and bezel. Only where the window is glass
        // already; a keyed, strip or fullscreen window keeps its behaviour. The
        // panes sit at rest, as the live path lays them out at the target.
        let takes_panes = window_takes_panes(
            mode,
            keyed.is_some(),
            strip.is_some(),
            fills_area(window),
            effects::fb_y_mirrored(self.output.current_transform()).is_some(),
        );
        let pane = takes_panes
            .then(|| WaylandFocus::wl_surface(window))
            .flatten()
            .and_then(|surface| {
                let set = committed_panes(&surface)?;
                let store = &mut *self.store;
                ensure_glass(renderer, store).then_some(())?;
                let size = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<RendererSurfaceStateUserData>()
                        .and_then(|d| d.lock().unwrap().surface_size())
                })
                .unwrap_or_else(|| window.bbox().size);
                Some((set, size))
            });

        let out_rect = Rectangle::new(
            Point::from((0, 0)),
            self.output_geo.size.to_f64().to_physical(scale).to_i32_round(),
        );
        let mut popups = popups;
        if keyed.is_none() {
            for (_, _, _, els) in popup_groups.drain(..) {
                popups.extend(els);
            }
        }
        let push_keyed = |out: &mut Vec<AbyssRenderElement>,
                          program: &smithay::backend::renderer::gles::GlesTexProgram,
                          els: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
                          uniforms: Vec<smithay::backend::renderer::gles::Uniform<'static>>,
                          crop: Option<Rectangle<i32, Physical>>| {
            for s in els {
                let el = effects::RoundedElement::new(s, program.clone(), uniforms.clone());
                match crop {
                    Some(crop) => out.extend(
                        CropRenderElement::from_element(el, scale, crop)
                            .map(AbyssRenderElement::CroppedRounded),
                    ),
                    None => out.push(AbyssRenderElement::Rounded(el)),
                }
            }
        };
        if let Some((program, fb_h, mirrored, keys)) = &keyed {
            for (n, prect, pradius, els) in popup_groups.drain(..) {
                push_keyed(
                    out,
                    program,
                    els,
                    effects::keyed_uniforms(None, None, *fb_h, *mirrored, keys),
                    None,
                );
                if mode != BlurMode::Off {
                    self.requests.push((
                        blur::BlurKey::Popup(window.clone(), n),
                        out.len(),
                        prect,
                        mode,
                        pradius,
                        None,
                        None,
                        None,
                    ));
                }
            }
        }
        push_popups(out, popups, scale, None);
        // Below its popups and level with its surfaces, as in `window_elements`.
        // A redacted window never reaches this method, so its title is not drawn.
        if let Some(info) = bar {
            let at = crate::titlebar::bar_rect(geo, info.height);
            let radius = match &self.rounding {
                Some(_) if !info.square => deco.rounding,
                _ => 0,
            };
            if let Some(element) = crate::titlebar::bar_element(
                renderer,
                &mut self.store.bars,
                self.output,
                window,
                info,
                phys(at.loc - output_loc, scale),
                at.size.w,
                scale,
                active,
                radius,
                alpha,
                self.config.appearance.glass_light() > 0.0,
            ) {
                out.push(AbyssRenderElement::Memory(element));
            }
        }
        // Each band is the surfaces drawn within one crop: the whole tree, or
        // for a strip the strip's copy above its bottom edge and the plain
        // one below it.
        let bands = match strip {
            Some((strip, copy)) if keyed.is_none() => {
                let [above, below] = strip_crops(strip, out_rect, None);
                vec![(copy, Some(above)), (surfaces, Some(below))]
            }
            _ => vec![(surfaces, None)],
        };
        for (surfaces, crop) in bands {
            if let Some((program, fb_h, mirrored, keys)) = &keyed {
                let radius = deco.rounding as f64 * scale.x.max(scale.y);
                let mask = self.rounding.as_ref().map(|_| (frame_rect, radius as f32));
                let region = key_region;
                push_keyed(
                    out,
                    program,
                    surfaces,
                    effects::keyed_uniforms(mask, region, *fb_h, *mirrored, keys),
                    crop,
                );
                continue;
            }
            match (&self.rounding, crop) {
                (Some(((fb_height, mirrored), program)), crop) => {
                    let radius = deco.rounding as f64 * scale.x.max(scale.y);
                    let uniforms =
                        effects::rounding_uniforms(frame_rect, *fb_height, *mirrored, radius as f32);
                    let round = |s| effects::RoundedElement::new(s, program.clone(), uniforms.clone());
                    match crop {
                        Some(crop) => out.extend(
                            surfaces
                                .into_iter()
                                .filter_map(|s| CropRenderElement::from_element(round(s), scale, crop))
                                .map(AbyssRenderElement::CroppedRounded),
                        ),
                        None => out.extend(
                            surfaces
                                .into_iter()
                                .map(|s| AbyssRenderElement::Rounded(round(s))),
                        ),
                    }
                }
                (None, Some(crop)) => out.extend(
                    surfaces
                        .into_iter()
                        .filter_map(|s| CropRenderElement::from_element(s, scale, crop))
                        .map(AbyssRenderElement::Cropped),
                ),
                (None, None) => out.extend(surfaces.into_iter().map(AbyssRenderElement::Surface)),
            }
        }

        // Border, glow, then shadow, all directly under this window's surfaces
        // (COMP-02 §9), as the live path draws them. In glass and frost the
        // bezel is the frame, so no painted ring is drawn beside it.
        match &self.border {
            Some(_) if ready => {}
            Some(border) => {
                let color = if active {
                    self.config.general.col_active
                } else {
                    self.config.general.col_inactive
                };
                push_border(self.store, border, self.output, window, frame, color, out);
            }
            None => {}
        }
        if let Some(program) = &self.glow {
            let (on, off) = glow_colors(self.config);
            let color = if active { on } else { off };
            push_glow(
                self.store,
                program,
                self.output,
                window,
                frame,
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
                frame,
                output_loc,
                self.config,
                focus,
                out,
            );
        }

        // The panes go in above the window's backdrop and bezel, which the
        // request below splices in at the same index (`PaneRequest::before`).
        if let Some((set, size)) = pane {
            self.panes.push(PaneRequest {
                key: blur::BlurKey::Panes(window.clone()),
                index: out.len(),
                set,
                size,
                // The root surface's origin, output-local physical.
                loc: render_loc,
                crop: None,
                over: true,
                scaled: None,
                alpha: 1.0,
            });
        }
        if let Some((region, radius, bezel, shape)) = backdrop {
            self.requests.push((
                blur::BlurKey::Window(window.clone()),
                out.len(),
                region,
                mode,
                radius,
                bezel,
                shape,
                None,
            ));
        }
    }

    /// Push one layer surface at `geo` (output-local logical), with its
    /// backdrop when its glass shows, or its `eclipse_glass_v1` panes when it
    /// has any. Layers take the global blur mode.
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
        // `ext_background_effect_v1` replaces the layer rect and input shape;
        // an empty region is no blur, a scrim included.
        let explicit = explicit_blur(surface.wl_surface(), geo.size, loc, scale);
        // `eclipse_glass_v1` (GLASS-01): a surface with committed panes gets
        // glass under each pane and none under the whole surface, as the live
        // path draws it. Glass mode only, on an output the glass program can
        // draw to.
        let panes = (mode == BlurMode::Glass
            && effects::fb_y_mirrored(self.output.current_transform()).is_some())
        .then(|| committed_panes(surface.wl_surface()))
        .flatten()
        .filter(|_| ensure_glass(renderer, self.store));
        if let Some(set) = panes {
            let els = cropped(els, scale, crop);
            // Directly below the surface's own elements.
            let index = out.len() + els.len();
            // A shaped layer (the bar, a drawer) keeps its sheet glass, with its
            // panes above it; an unshaped one (toasts, launcher) is only panes.
            let sheet = explicit
                .is_none()
                .then(|| layer_shape(surface, geo.size, loc, scale, self.config))
                .flatten();
            let over = sheet.is_some();
            if let Some(shape) = sheet {
                self.requests.push((
                    blur::BlurKey::Layer(surface.clone()),
                    index,
                    shape.bounds().unwrap_or(region),
                    mode,
                    layer_radius(surface, self.config),
                    None,
                    Some(shape),
                    crop,
                ));
            }
            self.panes.push(PaneRequest {
                key: blur::BlurKey::LayerPanes(surface.clone()),
                index,
                set,
                size: geo.size,
                loc,
                crop,
                over,
                scaled: None,
                alpha: 1.0,
            });
            out.extend(els);
            return;
        }
        let wants = mode != BlurMode::Off
            && match &explicit {
                Some(area) => *area != blur::BlurArea::Nothing,
                None => !is_scrim(surface, geo.size, self.output_geo.size),
            };
        let (region, shape) = match explicit {
            Some(blur::BlurArea::Rect(r)) => (r, None),
            Some(blur::BlurArea::Shape(s)) => (s.bounds().unwrap_or(region), Some(s)),
            _ => (
                region,
                wants
                    .then(|| layer_shape(surface, geo.size, loc, scale, self.config))
                    .flatten(),
            ),
        };
        let shows = wants && layer_shows(region, shape.as_ref(), opaque_of(&els, scale));
        let els = cropped(els, scale, crop);
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

    /// Splice in every backdrop and pane set requested, each sampling only
    /// what lies behind it in `out`.
    pub fn finish(self, renderer: &mut GlesRenderer, out: &mut Vec<AbyssRenderElement>) {
        insert_blur(
            renderer,
            self.output,
            self.store,
            self.config,
            self.requests,
            self.panes,
            out,
        );
    }
}
