// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_background_effect_v1` (COMP-06 §1 addendum; BLUR-03).
//!
//! A client tells the compositor which part of a translucent surface should
//! have its backdrop blurred, instead of the compositor blurring the whole
//! window or layer rect. Smithay 0.7 has no module for it, so the global and
//! its dispatches are written out here against the wayland-protocols bindings,
//! following `output_power.rs`.
//!
//! State: the blur region is double-buffered surface state, kept as a smithay
//! [`Cacheable`](smithay::wayland::compositor::Cacheable)
//! ([`ec_abyss_render::blur::BackgroundEffectState`]) in the surface's cached
//! state. Requests only write the *pending* side; `wl_surface.commit` latches
//! it, and the renderer reads the *current* side straight from the surface
//! (`ec-abyss-render` stays state-free). A surface with no object has
//! `blur: None` and keeps the compositor's own backdrop choice; a live object
//! that never set a region, or set NULL, has the empty region, which is no blur.
//!
//! Capability: the manager advertises `blur` exactly while the compositor
//! applies blur at all (`decoration.blur.mode != off`). The protocol sends
//! capabilities on bind and on every change, and says an effect whose
//! capability is gone is no longer applied, so [`update_capabilities`] must be
//! called after a config change.

use std::sync::atomic::{AtomicBool, Ordering};

use ec_abyss_render::blur::BackgroundEffectState;
use smithay::{
    reexports::{
        wayland_protocols::ext::background_effect::v1::server::{
            ext_background_effect_manager_v1::{
                Capability, Error as ManagerError, ExtBackgroundEffectManagerV1, Request as ManagerRequest,
            },
            ext_background_effect_surface_v1::{
                Error as SurfaceError, ExtBackgroundEffectSurfaceV1, Request as SurfaceRequest,
            },
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            protocol::wl_surface::WlSurface,
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
        },
    },
    wayland::compositor::{get_region_attributes, with_states, RegionAttributes},
};

use crate::state::AbyssState;

const VERSION: u32 = 1;

/// On a surface's data map: whether it already has an effect object, which is
/// what `background_effect_exists` is raised from.
#[derive(Default)]
struct HasObject(AtomicBool);

/// User data on an `ext_background_effect_surface_v1`.
pub struct BackgroundEffectData {
    surface: Weak<WlSurface>,
    /// False for the duplicate that got `background_effect_exists`: it must
    /// not clear the flag the first object owns when it is destroyed.
    owner: bool,
}

pub struct BackgroundEffectGlobals {
    #[allow(dead_code)] // holds the global alive
    global: GlobalId,
    instances: Vec<ExtBackgroundEffectManagerV1>,
    /// The capability last announced.
    blur: bool,
}

impl BackgroundEffectGlobals {
    /// `blur` is whether the compositor blurs at all right now.
    pub fn new(display: &DisplayHandle, blur: bool) -> Self {
        let global = display.create_global::<AbyssState, ExtBackgroundEffectManagerV1, _>(VERSION, ());
        Self {
            global,
            instances: Vec::new(),
            blur,
        }
    }
}

fn flags(blur: bool) -> Capability {
    if blur {
        Capability::Blur
    } else {
        Capability::empty()
    }
}

/// Whether the live config applies blur: the capability to advertise.
fn blur_capable(state: &AbyssState) -> bool {
    state.config.decoration.blur.mode != crate::config::BlurMode::Off
}

/// Re-announce the capability to every manager if blur was turned on or off.
/// Call after a config change.
pub fn update_capabilities(state: &mut AbyssState) {
    let blur = blur_capable(state);
    let globals = &mut state.background_effect;
    globals.instances.retain(|m| m.is_alive());
    if globals.blur == blur {
        return;
    }
    globals.blur = blur;
    for manager in &globals.instances {
        manager.capabilities(flags(blur));
    }
}

impl GlobalDispatch<ExtBackgroundEffectManagerV1, ()> for AbyssState {
    fn bind(
        state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<ExtBackgroundEffectManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(manager, ());
        manager.capabilities(flags(blur_capable(state)));
        state.background_effect.instances.push(manager);
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        manager: &ExtBackgroundEffectManagerV1,
        request: ManagerRequest,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ManagerRequest::GetBackgroundEffect { id, surface } => {
                let owner = with_states(&surface, |states| {
                    let has = states.data_map.get_or_insert_threadsafe(HasObject::default);
                    !has.0.swap(true, Ordering::SeqCst)
                });
                if owner {
                    // The initial blur region is empty; it takes effect at the
                    // next commit like any other pending state.
                    with_states(&surface, |states| {
                        let mut cached = states.cached_state.get::<BackgroundEffectState>();
                        cached.pending().blur = Some(RegionAttributes::default());
                    });
                } else {
                    manager.post_error(
                        ManagerError::BackgroundEffectExists,
                        "the surface already has a background effect object",
                    );
                }
                data_init.init(
                    id,
                    BackgroundEffectData {
                        surface: surface.downgrade(),
                        owner,
                    },
                );
            }
            ManagerRequest::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ExtBackgroundEffectManagerV1, _data: &()) {
        state.background_effect.instances.retain(|m| m != manager);
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, BackgroundEffectData> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        object: &ExtBackgroundEffectSurfaceV1,
        request: SurfaceRequest,
        data: &BackgroundEffectData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            SurfaceRequest::SetBlurRegion { region } => {
                let Ok(surface) = data.surface.upgrade() else {
                    object.post_error(SurfaceError::SurfaceDestroyed, "the surface has been destroyed");
                    return;
                };
                if !data.owner {
                    return;
                }
                // Copy semantics: read the region now; the wl_region may be
                // destroyed at once. NULL removes the effect: the empty region.
                let attrs = region.as_ref().map(get_region_attributes).unwrap_or_default();
                with_states(&surface, |states| {
                    let mut cached = states.cached_state.get::<BackgroundEffectState>();
                    cached.pending().blur = Some(attrs);
                });
            }
            SurfaceRequest::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _object: &ExtBackgroundEffectSurfaceV1,
        data: &BackgroundEffectData,
    ) {
        // Explicit destroy or the client going away: the effect is removed on
        // the next commit, and the surface may get a new object. A dead
        // surface needs nothing.
        if !data.owner {
            return;
        }
        let Ok(surface) = data.surface.upgrade() else {
            return;
        };
        with_states(&surface, |states| {
            states.cached_state.get::<BackgroundEffectState>().pending().blur = None;
            if let Some(has) = states.data_map.get::<HasObject>() {
                has.0.store(false, Ordering::SeqCst);
            }
        });
    }
}
