// SPDX-License-Identifier: AGPL-3.0-only
//! Capture rendering with redaction (COMP-02 §7, §8).
//!
//! Capture never reuses the on-screen pass list. It builds its own, and a
//! surface classified sensitive is *excluded from that list* — a solid
//! placeholder of its geometry is drawn instead, so the pixels never reach the
//! target buffer at all. The decision is recorded in [`Redacted`] before any
//! GPU work, so tests can assert on it without reading pixels.
//!
//! Trusted UI (the capture indicator, the lock screen) is drawn by the
//! backends, never here, so it cannot be captured by construction.

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
        Bind, ExportMem, Offscreen,
    },
    desktop::{layer_map_for_output, Window},
    output::Output,
    reexports::{
        wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::{
            self, ExtImageCopyCaptureFrameV1,
        },
        wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::{Buffer as BufferCoord, Logical, Physical, Point, Rectangle, Scale, Transform},
    wayland::shell::wlr_layer::Layer,
};

use crate::{render::AbyssRenderElement, state::AbyssState};

/// The pixel format captured frames are handed to clients in. `Xbgr8888` maps
/// straight onto the GL ES read path (`RGBA`/`UNSIGNED_BYTE`), so the readback
/// needs no channel swizzle on the CPU.
pub const FORMAT: crate::backend::Fourcc = crate::backend::Fourcc::Xbgr8888;
/// The matching `wl_shm` format advertised to clients.
pub const SHM_FORMAT: smithay::reexports::wayland_server::protocol::wl_shm::Format =
    smithay::reexports::wayland_server::protocol::wl_shm::Format::Xbgr8888;

/// Opaque black. A redacted surface must not leak its shape's contents, and an
/// alpha-blended placeholder would let whatever is underneath show through.
const PLACEHOLDER: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Why a surface's pixels were withheld. Recorded alongside the rectangles so
/// the pass list says not just *what* was covered but *which rule* covered it —
/// the three fail-closed reasons are indistinguishable from the geometry alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactReason {
    /// The surface itself is classified sensitive — it is in `state.sensitive`
    /// or its `app_id` is in `capture.redact_app_id`.
    Surface,
    /// The semantic tree is older than the surface's current generation. Its
    /// rectangles describe content that is no longer on screen, so they are not
    /// trusted and the whole surface is covered instead (COMP-02 §7).
    StaleTree,
    /// A `secret` node is known to exist but no tree places it — either no tree
    /// arrived at all, or the tree that did arrive omits it. Nothing can be
    /// located, so everything is covered. A tree from an untrusted source may
    /// not talk the compositor out of a secret it already knows about (the
    /// ratchet: app-declared sensitivity may only raise a class, never lower
    /// it).
    UnplacedSecret,
    /// The tree named `secret` nodes, but none of them intersected the surface
    /// after clipping. A rectangle outside its own surface is nonsense from an
    /// untrusted source; fail closed rather than emit no placeholder at all.
    NodesOutOfBounds,
    /// The tree was present and current: these are its `secret` node rectangles
    /// (S-05 §4) — a sub-region of an otherwise capturable surface.
    Nodes,
}

/// One redaction decision, recorded when the pass list is built.
///
/// `rects` is in compositor-logical space and is never empty: every reason
/// above covers *something*, and a decision that covered nothing would be a
/// silent leak rather than a redaction.
#[derive(Debug, Clone, PartialEq)]
pub struct Redacted {
    pub app_id: Option<String>,
    pub rects: Vec<Rectangle<i32, Logical>>,
    pub reason: RedactReason,
}

/// What the compositor knows about one surface's semantic tree (COMP-09).
///
/// `Absent` is deliberately distinct from `Present` with an empty node list:
/// "we have never heard from this surface" and "this surface told us it has no
/// secrets" lead to different decisions below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticTree {
    Absent,
    Present {
        /// The surface generation the tree was built against.
        generation: u64,
        /// `secret` node rectangles, in surface-local logical coordinates.
        secret: Vec<Rectangle<i32, Logical>>,
    },
}

/// What must be covered for a surface on node-granularity grounds.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeVerdict {
    /// Nothing at node granularity. Surface-level rules still apply.
    None,
    /// Fail closed: cover the entire surface, for this reason.
    WholeSurface(RedactReason),
    /// Cover these surface-local rectangles.
    Nodes(Vec<Rectangle<i32, Logical>>),
}

/// The fail-closed node-redaction decision (COMP-02 §7, amended by VOL2 §A-10).
///
/// Kept separate from the element walk on purpose: it touches no renderer and
/// no Wayland object, so the corpus-C cases below are exercised directly. Arms
/// are in spec order, and each one that cannot place a rectangle confidently
/// escalates to the whole surface rather than trusting what it has.
pub fn resolve_nodes(tree: &SemanticTree, surface_generation: u64, secret_node_known: bool) -> NodeVerdict {
    match tree {
        // Stale first: a tree we *have* but cannot trust is the case a naive
        // implementation gets wrong, because it looks like a usable answer.
        SemanticTree::Present { generation, .. } if *generation < surface_generation => {
            NodeVerdict::WholeSurface(RedactReason::StaleTree)
        }
        // A known secret the tree cannot place is covered whole, whether the
        // tree is missing entirely or merely silent about it. The silent case
        // is the one that matters: it is the tree trying to lower a class.
        SemanticTree::Present { secret, .. } if secret.is_empty() && secret_node_known => {
            NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
        }
        SemanticTree::Absent if secret_node_known => NodeVerdict::WholeSurface(RedactReason::UnplacedSecret),
        SemanticTree::Present { secret, .. } if !secret.is_empty() => NodeVerdict::Nodes(secret.clone()),
        SemanticTree::Present { .. } | SemanticTree::Absent => NodeVerdict::None,
    }
}

/// Map surface-local node rectangles into compositor-logical space, clip them
/// to the surface, and escalate if nothing survives.
///
/// `origin` is where the surface tree's own (0, 0) lands on the output — *not*
/// the window's placed geometry origin, which differs by `geometry().loc` for
/// any client with CSD shadow insets. `clip` is the surface's full bounding box
/// in the same space, so a node sitting in the shadow margin is still covered.
/// The output transform is applied uniformly at render time, below this.
///
/// A rectangle straddling the edge is clipped, never dropped — dropping it
/// would uncover the half that is on screen. The empty-result escalation lives
/// here rather than at each call site so the "`rects` is never empty" invariant
/// on [`Redacted`] has exactly one place to hold.
fn node_rects(
    local: &[Rectangle<i32, Logical>],
    origin: Point<i32, Logical>,
    clip: Rectangle<i32, Logical>,
) -> (Vec<Rectangle<i32, Logical>>, RedactReason) {
    let rects: Vec<_> = local
        .iter()
        .filter_map(|r| Rectangle::new(r.loc + origin, r.size).intersection(clip))
        .filter(|r| r.size.w > 0 && r.size.h > 0)
        .collect();
    if rects.is_empty() {
        (vec![clip], RedactReason::NodesOutOfBounds)
    } else {
        (rects, RedactReason::Nodes)
    }
}

/// The semantic facts for a surface: its tree, and whether a `secret` node is
/// known to exist for it.
///
/// This is the seam COMP-09 fills. `protocols/semantic/` does not exist yet, so
/// today the honest answer for every surface is "no tree, and no secret node
/// known" — *not* "present and empty", which would tell [`resolve_nodes`] the
/// surface had affirmatively declared itself clean.
fn semantics_for(_state: &AbyssState, _surface: Option<&WlSurface>) -> (SemanticTree, u64, bool) {
    (SemanticTree::Absent, 0, false)
}

/// Where a serviced capture reports back to.
///
/// Two capture protocols share this queue and the whole servicing path below
/// (ADR 0030); only the wire events differ, so the difference is confined to
/// this enum rather than duplicated through `service`.
#[derive(Debug, Clone, PartialEq)]
pub enum Sink {
    /// `zwlr_screencopy_v1` frame.
    Wlr(ZwlrScreencopyFrameV1),
    /// `ext_image_copy_capture_v1` frame.
    Ext(ExtImageCopyCaptureFrameV1),
}

impl Sink {
    pub fn is_alive(&self) -> bool {
        match self {
            Sink::Wlr(f) => f.is_alive(),
            Sink::Ext(f) => f.is_alive(),
        }
    }

    /// Report a runtime failure. The client must destroy the frame after this.
    pub fn failed(&self) {
        match self {
            Sink::Wlr(f) => f.failed(),
            Sink::Ext(f) => f.failed(ext_image_copy_capture_frame_v1::FailureReason::Unknown),
        }
    }

    /// Report success: the protocol-specific metadata, then `ready`.
    fn ready(&self, region: Rectangle<i32, Physical>, with_damage: bool, now: std::time::Duration) {
        let (w, h) = (region.size.w.max(0) as u32, region.size.h.max(0) as u32);
        let secs = now.as_secs();
        let (hi, lo, nsec) = (
            (secs >> 32) as u32,
            (secs & 0xFFFF_FFFF) as u32,
            now.subsec_nanos(),
        );
        match self {
            Sink::Wlr(f) => {
                if with_damage {
                    f.damage(0, 0, w, h);
                }
                f.ready(hi, lo, nsec);
            }
            Sink::Ext(f) => {
                // The order the protocol requires: metadata, then ready.
                f.transform(wl_output::Transform::Normal);
                f.damage(0, 0, region.size.w, region.size.h);
                f.presentation_time(hi, lo, nsec);
                f.ready();
            }
        }
    }
}

/// A capture request waiting for a renderer. Holding this does not mean the
/// capture was authorised — nothing reaches this queue until the shared gate
/// in [`crate::protocols::standard::screencopy::decide`] has returned `Allow`.
pub struct Pending {
    pub sink: Sink,
    pub buffer: WlBuffer,
    pub output_id: u64,
    /// Region of the output to copy, output-local and physical.
    pub region: Rectangle<i32, Physical>,
    pub with_damage: bool,
}

/// Is this surface sensitive at *surface* granularity? Ratchet rule
/// (COMP-02 §7): every input may only *raise* the class, none can lower it, so
/// this stays a plain `or` — there is no branch in which a `false` term talks a
/// `true` term back down.
///
/// `tree_demands_surface` is the node path escalating: a stale or absent tree
/// (see [`resolve_nodes`]) promotes the whole surface, and an app declaring
/// itself unremarkable cannot undo that.
pub fn is_sensitive(
    in_set: bool,
    app_id: Option<&str>,
    redact: &[String],
    tree_demands_surface: bool,
) -> bool {
    in_set || app_id.is_some_and(|id| redact.iter().any(|r| r == id)) || tree_demands_surface
}

/// Record a newly mapped window in the sensitive set when config says its
/// `app_id` is secret. Called on map, never on the commit path.
pub fn mark_sensitive(state: &mut AbyssState, window: &Window) {
    if state.config.capture.redact_app_id.is_empty() {
        return;
    }
    let Some(surface) = crate::shell::window_surface(window) else {
        return;
    };
    let app_id = crate::protocols::standard::data_device::app_id_of(&surface);
    if is_sensitive(
        false,
        app_id.as_deref(),
        &state.config.capture.redact_app_id,
        false,
    ) {
        tracing::info!(
            app_id = app_id.as_deref().unwrap_or("<none>"),
            "surface marked sensitive"
        );
        state.sensitive.insert(surface);
    }
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// Build the capture pass list for one output, with redaction applied.
///
/// Returns the elements front-to-back and the redaction decisions taken. The
/// cursor, borders and trusted UI are deliberately absent: a capture target is
/// the shell's content, not the compositor's own chrome.
pub fn capture_elements(
    renderer: &mut GlesRenderer,
    state: &AbyssState,
    output: &Output,
) -> (Vec<AbyssRenderElement>, Vec<Redacted>) {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = state
        .space
        .output_geometry(output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let mut elements: Vec<AbyssRenderElement> = Vec::new();
    let mut redacted: Vec<Redacted> = Vec::new();
    let redact_ids = &state.config.capture.redact_app_id;

    layer_elements(
        renderer,
        state,
        output,
        [Layer::Overlay, Layer::Top],
        scale,
        &mut elements,
        &mut redacted,
    );

    // Topmost first. `space.elements()` is bottom-to-top.
    for window in state.space.elements().rev() {
        let Some(geo) = state.space.element_geometry(window) else {
            continue;
        };
        let surface = crate::shell::window_surface(window);
        let app_id = surface
            .as_ref()
            .and_then(crate::protocols::standard::data_device::app_id_of);
        let in_set = surface.as_ref().is_some_and(|s| state.sensitive.contains(s));
        // Where the surface tree's own (0, 0) lands, and the box it occupies.
        // `geo` is the *placed* rect; a client with CSD shadow insets draws
        // outside it, so covering `geo` alone would leave those pixels through.
        let origin = geo.loc - window.geometry().loc;
        let bbox = window.bbox();
        let cover = Rectangle::new(origin + bbox.loc, bbox.size);
        // Decided here, before any GPU work touches the surface.
        let (tree, generation, secret_known) = semantics_for(state, surface.as_ref());
        let verdict = resolve_nodes(&tree, generation, secret_known);
        let whole = match verdict {
            NodeVerdict::WholeSurface(reason) => Some(reason),
            _ => None,
        };
        let surface_level = in_set
            || app_id
                .as_deref()
                .is_some_and(|id| redact_ids.iter().any(|r| r == id));
        if is_sensitive(in_set, app_id.as_deref(), redact_ids, whole.is_some()) {
            // Skipping the walk entirely is what keeps the pixels off the
            // target: popups and subsurfaces inherit the parent's class here by
            // construction, because none of their elements are ever generated.
            // Surface-level wins the label when both rules fire — it is the
            // standing classification, the tree verdict only the day's reason.
            let reason = if surface_level {
                RedactReason::Surface
            } else {
                whole.unwrap_or(RedactReason::Surface)
            };
            redacted.push(Redacted {
                app_id,
                rects: vec![cover],
                reason,
            });
            elements.extend(placeholders(&[cover], output_loc, scale));
            continue;
        }
        if let NodeVerdict::Nodes(local) = verdict {
            let (rects, reason) = node_rects(&local, origin, cover);
            redacted.push(Redacted {
                app_id: app_id.clone(),
                rects: rects.clone(),
                reason,
            });
            // Pushed before the surface's own elements: the list is front-to-back,
            // so the placeholders occlude the nodes they cover.
            elements.extend(placeholders(&rects, output_loc, scale));
            if reason == RedactReason::NodesOutOfBounds {
                continue;
            }
        }
        let loc = origin - output_loc;
        elements.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    phys(loc, scale),
                    scale,
                    1.0,
                )
                .into_iter()
                .map(AbyssRenderElement::Surface),
        );
    }

    layer_elements(
        renderer,
        state,
        output,
        [Layer::Bottom, Layer::Background],
        scale,
        &mut elements,
        &mut redacted,
    );

    (elements, redacted)
}

#[allow(clippy::too_many_arguments)]
fn layer_elements(
    renderer: &mut GlesRenderer,
    state: &AbyssState,
    output: &Output,
    which: [Layer; 2],
    scale: Scale<f64>,
    elements: &mut Vec<AbyssRenderElement>,
    redacted: &mut Vec<Redacted>,
) {
    let map = layer_map_for_output(output);
    for layer in which {
        for surface in map.layers_on(layer).rev() {
            let Some(geo) = crate::shell::layer_geometry(&map, surface) else {
                continue;
            };
            let wl = surface.wl_surface();
            // Layer surfaces carry no `app_id`, so only the sensitive set and
            // the node path can raise them.
            let (tree, generation, secret_known) = semantics_for(state, Some(wl));
            let verdict = resolve_nodes(&tree, generation, secret_known);
            let whole = match verdict {
                NodeVerdict::WholeSurface(reason) => Some(reason),
                _ => None,
            };
            let in_set = state.sensitive.contains(wl);
            if is_sensitive(in_set, None, &state.config.capture.redact_app_id, whole.is_some()) {
                let reason = if in_set {
                    RedactReason::Surface
                } else {
                    whole.unwrap_or(RedactReason::Surface)
                };
                redacted.push(Redacted {
                    app_id: None,
                    rects: vec![geo],
                    reason,
                });
                elements.extend(placeholders(&[geo], Point::default(), scale));
                continue;
            }
            if let NodeVerdict::Nodes(local) = verdict {
                let (rects, reason) = node_rects(&local, geo.loc, geo);
                redacted.push(Redacted {
                    app_id: None,
                    rects: rects.clone(),
                    reason,
                });
                elements.extend(placeholders(&rects, Point::default(), scale));
                if reason == RedactReason::NodesOutOfBounds {
                    continue;
                }
            }
            elements.extend(
                surface
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                        renderer,
                        phys(geo.loc, scale),
                        scale,
                        1.0,
                    )
                    .into_iter()
                    .map(AbyssRenderElement::Surface),
            );
        }
    }
}

/// One opaque quad per redaction rectangle, front-to-back order preserved.
fn placeholders(
    rects: &[Rectangle<i32, Logical>],
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Vec<AbyssRenderElement> {
    rects
        .iter()
        .map(|geo| {
            let buffer = SolidColorBuffer::new(geo.size, PLACEHOLDER);
            AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                &buffer,
                phys(geo.loc - output_loc, scale),
                scale,
                1.0,
                Kind::Unspecified,
            ))
        })
        .collect()
}

/// How long after the last captured frame the indicator stays lit. Capture
/// clients pull frames continuously; this only has to outlast one frame
/// interval so the indicator does not flicker.
pub const INDICATOR_LINGER: std::time::Duration = std::time::Duration::from_secs(2);

/// The compositor-drawn capture indicator (COMP-10 §3.6): a solid marker in
/// the top-right corner of every output, persistent for as long as capture is
/// happening. It is trusted UI, so it is drawn only by the backends into the
/// on-screen frame — never by [`capture_elements`], which is why a capture
/// client cannot see, capture or spoof it.
pub fn indicator(output: &Output, active: bool) -> Vec<AbyssRenderElement> {
    if !active {
        return Vec::new();
    }
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let logical: smithay::utils::Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    let size: smithay::utils::Size<i32, Logical> = (48, 8).into();
    let buffer = SolidColorBuffer::new(size, [1.0, 0.15, 0.15, 1.0]);
    let loc: Point<i32, Logical> = (logical.w - size.w - 8, 8).into();
    vec![AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        phys(loc, scale),
        scale,
        1.0,
        Kind::Unspecified,
    ))]
}

/// Service every queued capture. Called once per composite, from the backend
/// that owns the renderer — the only place a `GlesRenderer` is reachable.
pub fn service(state: &mut AbyssState, renderer: &mut GlesRenderer) {
    if state.captures.is_empty() {
        return;
    }
    let pending: Vec<Pending> = std::mem::take(&mut state.captures);
    for p in pending {
        if !p.sink.is_alive() {
            continue;
        }
        // Re-check the gate at service time: the session may have locked
        // between the capture request and this composite.
        if state.lock.locked {
            tracing::warn!("capture denied at service time (session locked)");
            p.sink.failed();
            continue;
        }
        match copy_one(state, renderer, &p) {
            Ok(()) => {
                let now = std::time::Duration::from(state.clock.now());
                p.sink.ready(p.region, p.with_damage, now);
                state.capture_seen = Some(std::time::Instant::now());
            }
            Err(e) => {
                tracing::warn!(error = %e, "capture failed");
                p.sink.failed();
            }
        }
    }
}

fn copy_one(state: &AbyssState, renderer: &mut GlesRenderer, p: &Pending) -> anyhow::Result<()> {
    let Some(entry) = state.outputs.get(p.output_id) else {
        anyhow::bail!("output gone");
    };
    let output = entry.output.clone();
    let mode = output
        .current_mode()
        .ok_or_else(|| anyhow::anyhow!("output has no mode"))?;

    let (elements, redacted) = capture_elements(renderer, state, &output);
    if !redacted.is_empty() {
        let rects: usize = redacted.iter().map(|r| r.rects.len()).sum();
        tracing::info!(
            decisions = redacted.len(),
            rects,
            "redaction applied to capture pass list"
        );
        for r in &redacted {
            tracing::debug!(
                app_id = r.app_id.as_deref().unwrap_or("<none>"),
                reason = ?r.reason,
                rects = r.rects.len(),
                "redaction decision"
            );
        }
    }

    // Render through the output's own transform so a capture is oriented the way
    // the user sees the screen, not the way the GPU happens to store it.
    let fb_size = output.current_transform().transform_size(mode.size);
    let mut target: smithay::backend::renderer::gles::GlesTexture = Offscreen::create_buffer(
        renderer,
        FORMAT,
        fb_size.to_logical(1).to_buffer(1, Transform::Normal),
    )?;
    {
        let mut fb = Bind::bind(renderer, &mut target)?;
        let mut tracker = OutputDamageTracker::from_output(&output);
        tracker
            .render_output(renderer, &mut fb, 0, &elements, PLACEHOLDER)
            .map_err(|e| anyhow::anyhow!("render capture: {e:?}"))?;
        let region: Rectangle<i32, BufferCoord> = Rectangle::new(
            (p.region.loc.x, p.region.loc.y).into(),
            (p.region.size.w, p.region.size.h).into(),
        );
        let mapping = ExportMem::copy_framebuffer(renderer, &fb, region, FORMAT)?;
        let pixels = ExportMem::map_texture(renderer, &mapping)?;
        write_shm(&p.buffer, p.region, pixels)?;
    }
    Ok(())
}

/// Copy the readback into the client's shm buffer. The buffer was validated
/// when `copy` arrived; re-validate here because the client may have resized
/// the pool in between.
fn write_shm(buffer: &WlBuffer, region: Rectangle<i32, Physical>, pixels: &[u8]) -> anyhow::Result<()> {
    let w = region.size.w.max(0) as usize;
    let h = region.size.h.max(0) as usize;
    let src_stride = w * 4;
    smithay::wayland::shm::with_buffer_contents_mut(buffer, |ptr, len, data| {
        if data.format != SHM_FORMAT || data.width as usize != w || data.height as usize != h {
            anyhow::bail!("buffer no longer matches the advertised frame");
        }
        let dst_stride = data.stride.max(0) as usize;
        let offset = data.offset.max(0) as usize;
        if dst_stride < src_stride || offset + dst_stride * h > len || pixels.len() < src_stride * h {
            anyhow::bail!("buffer too small");
        }
        for y in 0..h {
            // SAFETY: bounds checked immediately above; `ptr` is valid for `len`
            // bytes for the duration of this closure.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pixels.as_ptr().add(y * src_stride),
                    ptr.add(offset + y * dst_stride),
                    src_stride,
                );
            }
        }
        Ok(())
    })??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_sensitive, node_rects, resolve_nodes, NodeVerdict, RedactReason, SemanticTree};
    use smithay::utils::{Logical, Rectangle};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn present(generation: u64, secret: &[Rectangle<i32, Logical>]) -> SemanticTree {
        SemanticTree::Present {
            generation,
            secret: secret.to_vec(),
        }
    }

    #[test]
    fn nothing_is_sensitive_by_default() {
        assert!(!is_sensitive(false, Some("firefox"), &[], false));
        assert!(!is_sensitive(false, None, &[], false));
    }

    #[test]
    fn config_raises() {
        let redact = vec!["bitwarden".to_string()];
        assert!(is_sensitive(false, Some("bitwarden"), &redact, false));
        assert!(!is_sensitive(false, Some("firefox"), &redact, false));
    }

    #[test]
    fn the_set_raises_and_config_cannot_lower_it() {
        // Already in the sensitive set: no config value, and no missing
        // app_id, can bring it back down. Ratchet rule.
        assert!(is_sensitive(true, None, &[], false));
        assert!(is_sensitive(
            true,
            Some("firefox"),
            &["bitwarden".to_string()],
            false
        ));
    }

    #[test]
    fn the_tree_raises_and_nothing_can_lower_it() {
        // COMP-02 §7 ratchet: a stale or absent tree promotes the whole
        // surface, and an unremarkable app_id is not an argument against it.
        assert!(is_sensitive(false, None, &[], true));
        assert!(is_sensitive(false, Some("firefox"), &[], true));
    }

    // ---- corpus C (COMP-15, VOL2 §5) -------------------------------------

    #[test]
    fn stale_tree_redacts_the_whole_surface() {
        // The tree describes content that has already been replaced. Its
        // rectangles are not trusted, even though they are *present*.
        let tree = present(3, &[rect(10, 10, 20, 20)]);
        assert_eq!(
            resolve_nodes(&tree, 4, false),
            NodeVerdict::WholeSurface(RedactReason::StaleTree)
        );
        // Stale beats every other consideration, including "no secret known".
        assert_eq!(
            resolve_nodes(&present(0, &[]), 9, false),
            NodeVerdict::WholeSurface(RedactReason::StaleTree)
        );
    }

    #[test]
    fn absent_tree_with_a_known_secret_redacts_the_whole_surface() {
        // Corpus C names this case: something secret exists and nothing can
        // place it, so everything goes.
        assert_eq!(
            resolve_nodes(&SemanticTree::Absent, 7, true),
            NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
        );
    }

    #[test]
    fn absent_tree_with_no_known_secret_is_not_node_redaction() {
        // Surface-level rules still apply on top of this; the node path just
        // has nothing to say. This is today's runtime state for every surface.
        assert_eq!(resolve_nodes(&SemanticTree::Absent, 7, false), NodeVerdict::None);
    }

    #[test]
    fn a_current_tree_yields_its_secret_nodes() {
        let nodes = [rect(4, 8, 16, 32)];
        assert_eq!(
            resolve_nodes(&present(5, &nodes), 5, true),
            NodeVerdict::Nodes(nodes.to_vec())
        );
    }

    #[test]
    fn a_current_tree_that_declares_no_secrets_redacts_nothing() {
        // Distinct from `Absent`: the surface affirmatively said it is clean
        // at a generation we still believe, and we know of no secret to place.
        assert_eq!(resolve_nodes(&present(5, &[]), 5, false), NodeVerdict::None);
    }

    #[test]
    fn a_tree_cannot_omit_a_secret_the_compositor_already_knows_about() {
        // The ratchet, inverted, is the bug this guards: an untrusted tree
        // that simply declines to mention the secret node would otherwise
        // talk the compositor out of a redaction it had already decided on.
        assert_eq!(
            resolve_nodes(&present(5, &[]), 5, true),
            NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
        );
    }

    #[test]
    fn a_newer_tree_than_the_surface_is_not_stale() {
        // The tree can legitimately run ahead of the generation the render
        // path last observed; only *older* is untrustworthy.
        let nodes = [rect(0, 0, 4, 4)];
        assert_eq!(
            resolve_nodes(&present(6, &nodes), 5, false),
            NodeVerdict::Nodes(nodes.to_vec())
        );
    }

    #[test]
    fn nodes_move_with_the_surface() {
        let geo = rect(100, 50, 200, 200);
        assert_eq!(
            node_rects(&[rect(10, 10, 20, 20)], geo.loc, geo),
            (vec![rect(110, 60, 20, 20)], RedactReason::Nodes)
        );
    }

    #[test]
    fn nodes_are_placed_against_the_surface_origin_not_the_placed_geometry() {
        // A client with CSD shadow insets draws from (90, 40) while its
        // window geometry sits at (100, 50). Node coordinates are relative to
        // the surface tree's own (0, 0) — placing them against `geo.loc`
        // instead displaces every quad by the inset and uncovers the secret.
        let origin = (90, 40).into();
        let clip = rect(90, 40, 220, 220);
        assert_eq!(
            node_rects(&[rect(10, 10, 20, 20)], origin, clip),
            (vec![rect(100, 50, 20, 20)], RedactReason::Nodes)
        );
    }

    #[test]
    fn nodes_are_clipped_to_the_surface_never_dropped() {
        let geo = rect(0, 0, 100, 100);
        // Straddles the right edge: the on-screen half must stay covered.
        assert_eq!(
            node_rects(&[rect(80, 10, 40, 10)], geo.loc, geo),
            (vec![rect(80, 10, 20, 10)], RedactReason::Nodes)
        );
    }

    #[test]
    fn nodes_wholly_outside_the_surface_escalate_to_the_whole_surface() {
        // Nonsense from an untrusted source: cover everything rather than
        // emit no placeholder at all.
        let geo = rect(0, 0, 100, 100);
        assert_eq!(
            node_rects(&[rect(500, 500, 10, 10)], geo.loc, geo),
            (vec![geo], RedactReason::NodesOutOfBounds)
        );
        assert_eq!(
            node_rects(&[rect(-40, 0, 10, 10)], geo.loc, geo),
            (vec![geo], RedactReason::NodesOutOfBounds)
        );
    }

    #[test]
    fn a_zero_area_node_is_not_a_redaction_rectangle() {
        // It covers nothing, so it cannot stand as the redaction — and a
        // decision that covered nothing would be a silent leak.
        let geo = rect(0, 0, 100, 100);
        assert_eq!(
            node_rects(&[rect(10, 10, 0, 40)], geo.loc, geo),
            (vec![geo], RedactReason::NodesOutOfBounds)
        );
    }
}
