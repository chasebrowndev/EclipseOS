// SPDX-License-Identifier: AGPL-3.0-only
//! The render thread, one per window (T-00 §4.2, T-07 §1-§3). It sleeps on a channel until
//! poked, so a static screen costs no wakeups and no timers beyond the one the scheduler
//! asks for. It snapshots dirty rows under a short term lock, builds the scene without the
//! lock, and presents through the wgpu surface (which commits the `wl_surface`; main never
//! commits it). Frame callbacks and presentation feedback come from the `apoth-wl` pacer.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use apoth_font::system::FontSystem;
use apoth_grid::{Damage, FrameSnapshot};
use apoth_render::colour::{Palette, Rgb, TextBlending};
use apoth_render::cursor::{self, CursorScene, CursorShape, CursorState};
use apoth_render::gpu::{GpuOptions, RenderError};
use apoth_render::presenter::{PresentOutcome, Presenter, SurfaceOptions, SurfacePresenter};
use apoth_render::renderer::{FrameInput, Renderer, RendererConfig};
use apoth_render::scene::{BuildCtx, GridScene, SceneConfig};
use apoth_render::sched::{Pacing, Poll, SchedConfig, Scheduler};
use apoth_render::surface::{AlphaRequest, FaultAction, fault_action};
use apoth_wl::pace::CommitCtl;
use apoth_wl::{PaceEvent, Pacer, WlSurfaceHandle};

use crate::glyphs::{COLOR_PAGES, Glyphs, MASK_PAGES, PAGE};
use crate::pane::{MainMsg, PaneShared, RenderMsg, now_ns};

/// Settings the render thread needs.
#[derive(Clone, Debug)]
pub(crate) struct RenderSettings {
    pub(crate) padding: (u32, u32),
    pub(crate) opacity: f32,
    pub(crate) blending: TextBlending,
    pub(crate) bold_bright: bool,
    pub(crate) allow_software: bool,
    pub(crate) echo_window: Duration,
    pub(crate) sync_timeout: Duration,
    pub(crate) cursor_shape: CursorShape,
    pub(crate) unfocused_cursor_hidden: bool,
}

/// Consecutive reconfigure or recreate faults before the render thread gives up.
const MAX_FAULT_STREAK: u32 = 30;

pub(crate) struct RenderArgs {
    pub(crate) surface: WlSurfaceHandle,
    pub(crate) pacer: Pacer,
    pub(crate) shared: Arc<PaneShared>,
    pub(crate) fonts: FontSystem,
    pub(crate) settings: RenderSettings,
    pub(crate) scale: f64,
    pub(crate) buffer: (u32, u32),
    pub(crate) rx: mpsc::Receiver<RenderMsg>,
    /// A sender into `rx`: the pacer's wake-up for events that arrive on the render queue.
    pub(crate) poke: mpsc::Sender<RenderMsg>,
    pub(crate) to_main: calloop::channel::Sender<MainMsg>,
}

/// Padding in physical pixels at `scale`; main uses the same rule for the grid size.
pub(crate) fn pad_px(padding: u32, scale: f64) -> u32 {
    (f64::from(padding) * scale).round() as u32
}

struct Stack {
    presenter: SurfacePresenter,
    renderer: Renderer,
}

fn build_stack(
    surface: &Arc<WlSurfaceHandle>,
    size: (u32, u32),
    rs: &RenderSettings,
    cell: (u32, u32),
    scale: f64,
) -> Result<Stack, RenderError> {
    let opts = SurfaceOptions {
        gpu: GpuOptions {
            allow_software: rs.allow_software,
            compositor_hint: None,
        },
        alpha: AlphaRequest::Auto,
    };
    let presenter = SurfacePresenter::new(Arc::clone(surface), size, &opts)?;
    let cfg = RendererConfig {
        cell,
        target_format: presenter.format(),
        page_size: PAGE,
        mask_pages: MASK_PAGES as u32,
        color_pages: COLOR_PAGES as u32,
        text_blending: rs.blending,
        origin: (
            pad_px(rs.padding.0, scale) as i32,
            pad_px(rs.padding.1, scale) as i32,
        ),
    };
    let renderer = Renderer::new(presenter.gpu(), &cfg);
    Ok(Stack {
        presenter,
        renderer,
    })
}

fn shape_of(code: u8, default: CursorShape) -> CursorShape {
    match code {
        1 | 2 => CursorShape::Block,
        3 | 4 => CursorShape::Underline,
        5 | 6 => CursorShape::Beam,
        _ => default,
    }
}

pub(crate) fn spawn(a: RenderArgs) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("apoth-render".into())
        .spawn(move || run(a))
}

#[allow(clippy::too_many_lines)]
fn run(a: RenderArgs) {
    let RenderArgs {
        surface,
        mut pacer,
        shared,
        mut fonts,
        settings: rs,
        mut scale,
        mut buffer,
        rx,
        poke,
        to_main,
    } = a;
    pacer.set_wake(move || {
        let _ = poke.send(RenderMsg::Poke);
    });
    let ctl = Arc::new(CommitCtl::default());
    let surface = Arc::new(surface);
    let px = fonts.config().pixel_size(scale as f32);
    let Some(m) = fonts.cell_metrics(px) else {
        let _ = to_main.send(MainMsg::RenderFailed("no usable font".into()));
        return;
    };
    let mut glyphs = Glyphs::new(fonts, m, px);
    let cell = |g: &Glyphs| (g.metrics().width as u32, g.metrics().height as u32);
    let mut stack = match build_stack(&surface, buffer, &rs, cell(&glyphs), scale) {
        Ok(s) => s,
        Err(e) => {
            let _ = to_main.send(MainMsg::RenderFailed(e.to_string()));
            return;
        }
    };
    stack
        .presenter
        .set_pre_present(pacer.commit_hook(Arc::clone(&ctl)));
    let info = stack.presenter.info();
    tracing::info!(
        present_mode = ?info.present_mode,
        alpha = ?info.alpha,
        opaque_fallback = info.opaque_fallback,
        "presenting"
    );
    let mut sched = Scheduler::new(
        SchedConfig {
            echo_window: rs.echo_window,
            sync_timeout: rs.sync_timeout,
            ..SchedConfig::default()
        },
        Pacing::choose(true, info.present_mode),
    );
    let mut scene = GridScene::new();
    let mut snap = FrameSnapshot::new();
    let mut cscene = CursorScene::default();
    let mut pal = Palette::xterm();
    let scene_cfg = SceneConfig {
        bold_bright: rs.bold_bright,
        bg_alpha: rs.opacity,
        ..SceneConfig::default()
    };
    let (mut seen_key, mut was_hold, mut seen_focus) = (0u64, false, false);
    let mut fault_streak = 0u32;
    sched.damage(now_ns());

    'outer: loop {
        let now = now_ns();
        let mut surface_event = false;
        while let Some(ev) = pacer.try_recv() {
            match ev {
                PaceEvent::Frame { .. } => sched.frame_callback(),
                PaceEvent::Presented(p) => {
                    sched.presentation_feedback(Duration::from_nanos(u64::from(p.refresh_ns)));
                }
                PaceEvent::Discarded { .. } => {}
                PaceEvent::Surface {
                    buffer: b,
                    scale: s,
                    suspended,
                    ..
                } => {
                    surface_event = true;
                    sched.set_suspended(suspended, now);
                    let s = s.as_f64();
                    if b != buffer {
                        buffer = b;
                        stack.presenter.resize(b);
                        sched.damage(now);
                    }
                    if (s - scale).abs() > f64::EPSILON {
                        scale = s;
                        let px = glyphs.fonts.config().pixel_size(scale as f32);
                        if glyphs.set_px(px) {
                            let c = cell(&glyphs);
                            stack.renderer.set_cell(c);
                            stack.renderer.set_origin((
                                pad_px(rs.padding.0, scale) as i32,
                                pad_px(rs.padding.1, scale) as i32,
                            ));
                            stack.renderer.invalidate_all();
                            scene = GridScene::new();
                            let _ = to_main.send(MainMsg::CellSize(c));
                            sched.damage(now);
                        }
                    }
                }
            }
        }
        if surface_event && pacer.ack_pending() {
            // A configure waits for its ack: draw a frame at the new size to carry it.
            sched.damage(now);
        }
        let k = shared.last_key_ns.load(Ordering::Acquire);
        if k != seen_key {
            seen_key = k;
            sched.note_key(k);
        }
        if shared.dirty.swap(false, Ordering::AcqRel) {
            sched.damage(now);
        }
        let h = shared.hold.load(Ordering::Acquire);
        if h != was_hold {
            was_hold = h;
            if h {
                sched.sync_begin(now);
            } else {
                sched.sync_end(now);
            }
        }
        let f = shared.focused.load(Ordering::Acquire);
        if f != seen_focus {
            seen_focus = f;
            sched.damage(now);
        }

        let msg = match sched.poll(now_ns()) {
            Poll::Render(r) => {
                glyphs.begin_frame();
                let (damage, cur, colors) = {
                    let mut t = shared.lock();
                    let d = t.grid_mut().snapshot_dirty(&mut snap);
                    for i in 0..=255u8 {
                        let (r, g, b) = t.palette(i);
                        pal.indexed[usize::from(i)] = Rgb(r, g, b);
                    }
                    (d, t.cursor(), t.colors())
                };
                pal.fg = Rgb(colors.0.0, colors.0.1, colors.0.2);
                pal.bg = Rgb(colors.1.0, colors.1.1, colors.1.2);
                pal.cursor = Some(Rgb(colors.2.0, colors.2.1, colors.2.2));
                let metrics = glyphs.metrics();
                let ctx = BuildCtx {
                    cfg: &scene_cfg,
                    palette: &pal,
                    metrics: &metrics,
                    highlights: &[],
                };
                let mut work = scene.update(&snap, damage, &ctx, r.echo, &mut glyphs);
                let mut retries = 0;
                while std::mem::take(&mut glyphs.flushed) {
                    // An atlas was cleared mid-update: every row must be rebuilt. If the
                    // rebuild overflows too, the last pass draws what does not fit as blank
                    // (it cannot clear again), so no row keeps slots another glyph reuses.
                    retries += 1;
                    glyphs.degraded = retries >= 2;
                    stack.renderer.invalidate_all();
                    work = scene.update(&snap, Damage::Full, &ctx, r.echo, &mut glyphs);
                }
                let focused = shared.focused.load(Ordering::Acquire);
                let show_cursor = cur.visible
                    && cur.y < snap.rows.len()
                    && !(rs.unfocused_cursor_hidden && !focused);
                if show_cursor {
                    // The cursor glyph must not clear an atlas the rows were just built on.
                    glyphs.degraded = true;
                    let st = CursorState {
                        col: cur.x,
                        row: cur.y,
                        shape: shape_of(cur.style.shape, rs.cursor_shape),
                        visible: true,
                        focused,
                    };
                    cursor::build(
                        &st,
                        &snap.rows[cur.y],
                        &ctx,
                        r.echo,
                        &mut glyphs,
                        &mut cscene,
                    );
                }
                glyphs.degraded = false;
                glyphs.flush(&stack.renderer);
                let a = rs.opacity;
                let clear = [
                    f32::from(pal.bg.0) / 255.0 * a,
                    f32::from(pal.bg.1) / 255.0 * a,
                    f32::from(pal.bg.2) / 255.0 * a,
                    a,
                ];
                // The pre-present hook sends ack, frame request and feedback, and only for a
                // frame that is actually committed.
                ctl.set_frame(r.request_frame);
                ctl.set_size(stack.presenter.size());
                let input = FrameInput {
                    scene: &scene,
                    cursor: show_cursor.then_some(&cscene),
                    clear,
                };
                match stack.presenter.present(&mut stack.renderer, &input) {
                    PresentOutcome::Presented(st) => {
                        fault_streak = 0;
                        sched.rendered();
                        tracing::trace!(
                            rows = st.draw.rows_drawn,
                            glyphs = st.draw.glyphs,
                            full = st.draw.full,
                            "frame"
                        );
                    }
                    PresentOutcome::Skipped(fault) => {
                        tracing::debug!(?fault, "frame skipped");
                        match fault_action(fault) {
                            FaultAction::Abort => {
                                let _ = to_main.send(MainMsg::RenderFailed(format!(
                                    "surface fault: {fault:?}"
                                )));
                                break 'outer;
                            }
                            FaultAction::Reconfigure | FaultAction::RecreateSurface => {
                                // The presenter already reconfigured or recreated the
                                // swapchain; redraw at the known size. A fault that keeps
                                // coming back is fatal rather than a silent spin.
                                fault_streak += 1;
                                if fault_streak > MAX_FAULT_STREAK {
                                    let _ = to_main.send(MainMsg::RenderFailed(format!(
                                        "surface fault persists: {fault:?}"
                                    )));
                                    break 'outer;
                                }
                                stack.presenter.resize(buffer);
                                sched.skipped(now_ns());
                            }
                            FaultAction::RebuildDevice | FaultAction::SkipFrame => {
                                sched.skipped(now_ns());
                            }
                        }
                    }
                    PresentOutcome::DeviceLost => {
                        tracing::warn!("device lost; rebuilding the renderer");
                        glyphs.reset();
                        glyphs.flushed = false;
                        scene = GridScene::new();
                        match build_stack(&surface, buffer, &rs, cell(&glyphs), scale) {
                            Ok(s) => {
                                stack = s;
                                stack
                                    .presenter
                                    .set_pre_present(pacer.commit_hook(Arc::clone(&ctl)));
                                sched.damage(now_ns());
                            }
                            Err(e) => {
                                let _ = to_main.send(MainMsg::RenderFailed(e.to_string()));
                                break 'outer;
                            }
                        }
                    }
                }
                if work.pending > 0 {
                    sched.damage(now_ns());
                }
                continue;
            }
            Poll::Idle | Poll::Wait(None) => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Poll::Wait(Some(t)) => {
                rx.recv_timeout(Duration::from_nanos(t.saturating_sub(now_ns())))
            }
        };
        match msg {
            Ok(RenderMsg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(RenderMsg::Poke) | Err(RecvTimeoutError::Timeout) => {}
        }
    }
    // Swapchain before the window roles (INTEGRATION-61): dropped here, before main destroys
    // the window.
    drop(stack);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_scales_and_rounds() {
        assert_eq!(pad_px(6, 1.0), 6);
        assert_eq!(pad_px(6, 1.25), 8);
        assert_eq!(pad_px(0, 2.0), 0);
    }

    /// T-23 snapshot-lock bound: a full 300x80 snapshot under the term lock takes <= 100 us.
    /// Timing is meaningless unoptimised, so this runs only with
    /// `cargo test --release -p apotheosis -- --ignored snapshot_lock`. Measured at M11 fix
    /// round 1: p99 about 385 us, so the bound is NOT met yet (apoth-grid snapshot cost).
    #[test]
    #[ignore = "timing bound; run in release"]
    fn snapshot_lock_300x80_within_100us() {
        let mut t = apoth_term::Term::new(apoth_term::TermConfig {
            rows: 80,
            cols: 300,
            ..apoth_term::TermConfig::default()
        });
        let line = "x".repeat(300);
        for _ in 0..80 {
            t.feed(line.as_bytes(), std::time::Instant::now());
        }
        let mut snap = FrameSnapshot::new();
        let mut times = Vec::new();
        for _ in 0..200 {
            t.grid_mut().mark_full();
            let t0 = std::time::Instant::now();
            let _ = t.grid_mut().snapshot_dirty(&mut snap);
            times.push(t0.elapsed());
        }
        times.sort();
        let p99 = times[times.len() * 99 / 100];
        assert!(p99 <= Duration::from_micros(100), "p99 {p99:?}");
    }

    #[test]
    fn decscusr_codes_map_to_shapes() {
        assert_eq!(shape_of(2, CursorShape::Beam), CursorShape::Block);
        assert_eq!(shape_of(4, CursorShape::Block), CursorShape::Underline);
        assert_eq!(shape_of(6, CursorShape::Block), CursorShape::Beam);
        assert_eq!(shape_of(0, CursorShape::Beam), CursorShape::Beam);
    }
}
