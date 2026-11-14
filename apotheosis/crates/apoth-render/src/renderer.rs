// SPDX-License-Identifier: AGPL-3.0-only
//! The wgpu executor (T-07 §4 passes 1, 3 and 5; RENDERING-01, -02, -03, -05, -09, -17, -18,
//! -19, -24, -25, -26; PERFORMANCE-14, -17).
//!
//! What it draws, back to front:
//!
//! 1. window fill: the clear colour of the present pass (premultiplied);
//! 2. images with negative z (and the pane backdrop they need), clipped to the pane;
//! 3. grid: the retained grid texture, composited through the row ring;
//! 4. images with z >= 0, clipped to the pane (T-15 §6, [`crate::image`]);
//! 5. cursor: rects, plus the glyph under a block cursor recoloured.
//!
//! The grid texture holds the finished cell pixels (backgrounds, glyphs, decorations) of the
//! whole grid. Only damaged rows are redrawn into it, each inside a scissor of its own row
//! band, so a glyph that overhangs its cell vertically is clipped to the row. The grid texture
//! and the background texture (one RGBA8 texel per cell) are row rings: logical row `y` lives
//! at physical row `(offset + y) % rows`, so a full-grid scroll only advances a uniform.
//! A region scroll goes through a scratch texture (`copy_texture_to_texture` is never an
//! overlapping in-place copy). Instance buffers are transient: the retained texture is what
//! persists, so only the damaged rows' instances are packed and uploaded each frame. The CPU
//! side of "per-row instance ranges move with their rows" is `scene::GridScene`.
//!
//! Background rows reach the GPU through a staging buffer copied inside the command encoder
//! (not `Queue::write_texture`): a queue write is ordered before the whole submission, which
//! would let it overwrite rows that a region copy in the same frame still has to read.
//!
//! Text blending (`render.text-blending`) happens in the glyph fragment shader against the
//! cell's background texel, mirroring `colour::blend_coverage`. Glyph quads blend against the
//! cell background, not against neighbouring glyphs; they only differ where two glyph boxes
//! overlap, which a grid does not produce.
//!
//! A frame that was planned but not drawn (occluded, lost surface) leaves the retained texture
//! behind the scene. Presenters call [`Renderer::invalidate_all`] on every skipped frame and
//! the next draw redraws every row from the scene.

use crate::chrome::{ChromeList, ChromePass};
use crate::colour::TextBlending;
use crate::cursor::CursorScene;
use crate::damage::{Op, RowRing};
use crate::gpu::Gpu;
use crate::image::{Band, ImagePass, ImageQuad};
use crate::scene::{GlyphInstance, GridScene, RectInstance};

const SHADER: &str = include_str!("shaders/grid.wgsl");
const GRID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const UNIFORM_BYTES: usize = 64;
const GLYPH_STRIDE: u64 = 24;
const RECT_STRIDE: u64 = 20;
const MAX_CURSOR_RECTS: u64 = 8;

/// Executor configuration.
#[derive(Clone, Debug)]
pub struct RendererConfig {
    /// Cell size in pixels (integer cell metrics at the window's scale).
    pub cell: (u32, u32),
    /// Format of the target the present pass draws into (non-sRGB).
    pub target_format: wgpu::TextureFormat,
    /// Atlas page edge in pixels.
    pub page_size: u32,
    /// Pages of the R8 mask atlas.
    pub mask_pages: u32,
    /// Pages of the RGBA colour atlas.
    pub color_pages: u32,
    /// `render.text-blending`.
    pub text_blending: TextBlending,
    /// Where the grid's top-left sits in the target (window padding), pixels.
    pub origin: (i32, i32),
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            cell: (8, 16),
            target_format: wgpu::TextureFormat::Rgba8Unorm,
            page_size: 1024,
            mask_pages: 8,
            color_pages: 2,
            text_blending: TextBlending::LinearCorrected,
            origin: (0, 0),
        }
    }
}

/// One pane of a frame: its retained scene, cursor and where its grid sits.
#[derive(Clone, Copy, Debug)]
pub struct PaneDraw<'a> {
    /// Stable key of the pane: its retained grid texture lives under this key between frames
    /// (T-13: one window draws every pane of its shown tab). Keys absent from a frame keep
    /// their textures until [`Renderer::retain`] drops them.
    pub key: u64,
    /// The retained grid scene, already updated for this frame.
    pub scene: &'a GridScene,
    /// The cursor, if visible.
    pub cursor: Option<&'a CursorScene>,
    /// Top-left of the grid in the target, pixels; `None` uses [`RendererConfig::origin`].
    pub origin: Option<(i32, i32)>,
    /// Image placements under the text (negative z), bottom to top.
    pub images_below: &'a [ImageQuad<'a>],
    /// Image placements over the text, bottom to top.
    pub images_above: &'a [ImageQuad<'a>],
    /// Premultiplied fill drawn under `images_below` over the pane's grid rect: a pane with
    /// below-text images builds its scene with transparent default backgrounds, and this
    /// puts the window-opacity background back underneath the images.
    pub backdrop: Option<[f32; 4]>,
}

impl<'a> PaneDraw<'a> {
    /// The single-pane case: key 0 at the configured origin.
    #[must_use]
    pub fn single(scene: &'a GridScene, cursor: Option<&'a CursorScene>) -> Self {
        PaneDraw {
            key: 0,
            scene,
            cursor,
            origin: None,
            images_below: &[],
            images_above: &[],
            backdrop: None,
        }
    }
}

/// What one frame draws.
#[derive(Clone, Copy, Debug)]
pub struct FrameInput<'a> {
    /// The panes, back to front (tiled panes do not overlap).
    pub panes: &'a [PaneDraw<'a>],
    /// Window fill, premultiplied RGBA.
    pub clear: [f32; 4],
}

/// What a draw did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawStats {
    /// Rows redrawn into the retained texture.
    pub rows_drawn: usize,
    /// Glyph instances uploaded.
    pub glyphs: usize,
    /// Decoration rect instances uploaded.
    pub rects: usize,
    /// Region-scroll copies issued.
    pub region_copies: usize,
    /// Every row was redrawn (first frame, resize, or after a skipped frame).
    pub full: bool,
}

#[derive(Debug)]
struct DynBuf {
    buf: wgpu::Buffer,
    cap: u64,
    usage: wgpu::BufferUsages,
    label: &'static str,
}

impl DynBuf {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        cap: u64,
    ) -> Self {
        Self {
            buf: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: cap,
                usage,
                mapped_at_creation: false,
            }),
            cap,
            usage,
            label,
        }
    }

    fn ensure(&mut self, device: &wgpu::Device, need: u64) {
        if need > self.cap {
            let cap = need.next_power_of_two().max(4096);
            *self = Self::new(device, self.label, self.usage, cap);
        }
    }
}

#[derive(Debug)]
struct Targets {
    cols: u32,
    rows: u32,
    w: u32,
    h: u32,
    stride: u32,
    grid_tex: wgpu::Texture,
    grid_view: wgpu::TextureView,
    bg_tex: wgpu::Texture,
    scratch: Option<(wgpu::Texture, wgpu::Texture)>,
    stage: DynBuf,
    bind_grid: wgpu::BindGroup,
    bind_comp: wgpu::BindGroup,
    bind_cursor: wgpu::BindGroup,
}

/// A pane's retained grid and its own uniforms and instance buffers: every pane of a frame
/// is recorded into the same encoder, and queue writes land before the whole submission, so
/// panes cannot share a buffer.
#[derive(Debug)]
struct Slot {
    key: u64,
    targets: Option<Targets>,
    invalid: bool,
    ubo_grid: wgpu::Buffer,
    ubo_comp: wgpu::Buffer,
    ubo_cursor: wgpu::Buffer,
    glyph_vb: DynBuf,
    rect_vb: DynBuf,
    cursor_rect_vb: wgpu::Buffer,
    cursor_glyph_vb: wgpu::Buffer,
    /// This frame's cursor: rect count and whether a glyph is drawn.
    cursor_rects: u32,
    cursor_glyph: bool,
}

#[derive(Debug)]
struct Pipes {
    bg_grid: wgpu::RenderPipeline,
    glyph_grid: wgpu::RenderPipeline,
    rect_grid: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    glyph_cursor: wgpu::RenderPipeline,
    rect_cursor: wgpu::RenderPipeline,
}

#[derive(Clone, Copy, Debug)]
struct RowRange {
    phys: u32,
    g0: u32,
    g1: u32,
    r0: u32,
    r1: u32,
}

/// The uniform block shared by every pass (64 bytes; see `grid.wgsl`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Uni {
    target: (u32, u32),
    cell: (u32, u32),
    origin: (i32, i32),
    ring: (u32, u32),
    grid: (u32, u32),
    mode: u32,
    flags: u32,
    cursor_dst: [f32; 4],
}

impl Uni {
    fn bytes(&self) -> [u8; UNIFORM_BYTES] {
        let mut b = [0u8; UNIFORM_BYTES];
        let mut at = 0;
        let mut put = |v: [u8; 4]| {
            b[at..at + 4].copy_from_slice(&v);
            at += 4;
        };
        put((self.target.0 as f32).to_le_bytes());
        put((self.target.1 as f32).to_le_bytes());
        put(self.cell.0.to_le_bytes());
        put(self.cell.1.to_le_bytes());
        put(self.origin.0.to_le_bytes());
        put(self.origin.1.to_le_bytes());
        put(self.ring.0.to_le_bytes());
        put(self.ring.1.to_le_bytes());
        put(self.grid.0.to_le_bytes());
        put(self.grid.1.to_le_bytes());
        put(self.mode.to_le_bytes());
        put(self.flags.to_le_bytes());
        for c in self.cursor_dst {
            put(c.to_le_bytes());
        }
        b
    }
}

fn mode_of(m: TextBlending) -> u32 {
    match m {
        TextBlending::LinearCorrected => 0,
        TextBlending::Linear => 1,
        TextBlending::Native => 2,
    }
}

pub(crate) fn pack_glyph(g: &GlyphInstance, out: &mut Vec<u8>) {
    for v in g.cell {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in g.offset {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in g.atlas_xy {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in g.size {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&g.page.to_le_bytes());
    out.extend_from_slice(&g.flags.to_le_bytes());
    out.extend_from_slice(&g.color);
}

pub(crate) fn pack_rect(r: &RectInstance, y_add: i32, out: &mut Vec<u8>) {
    for v in [r.x, r.y + y_add, r.w, r.h] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&r.color);
}

/// The physical `(start, len)` runs of the logical rows `first..first + count` in a ring
/// (two runs when the range wraps; unused entries have length 0).
pub(crate) fn runs(ring: &RowRing, first: usize, count: usize) -> [(usize, usize); 2] {
    let rows = ring.rows();
    if count == 0 || rows == 0 {
        return [(0, 0); 2];
    }
    let start = ring.phys(first);
    if start + count <= rows {
        [(start, count), (0, 0)]
    } else {
        let a = rows - start;
        [(start, a), (0, count - a)]
    }
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    size: (u32, u32, u32),
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: size.2,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn copy_tex(
    enc: &mut wgpu::CommandEncoder,
    src: &wgpu::Texture,
    src_origin: (u32, u32),
    dst: &wgpu::Texture,
    dst_origin: (u32, u32),
    size: (u32, u32),
) {
    enc.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: src,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: src_origin.0,
                y: src_origin.1,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: dst,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: dst_origin.0,
                y: dst_origin.1,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
}

/// The wgpu executor. One per window: it owns that window's atlases and retained grid.
#[derive(Debug)]
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    cfg: RendererConfig,
    bgl: wgpu::BindGroupLayout,
    pipes: Pipes,
    mask_tex: wgpu::Texture,
    mask_view: wgpu::TextureView,
    color_tex: wgpu::Texture,
    color_view: wgpu::TextureView,
    dummy_view: wgpu::TextureView,
    /// One retained grid per pane key (T-13: a window draws every pane of its shown tab).
    slots: Vec<Slot>,
    rows_scratch: Vec<u32>,
    phys_scratch: Vec<u32>,
    ranges: Vec<RowRange>,
    glyph_bytes: Vec<u8>,
    rect_bytes: Vec<u8>,
    bg_bytes: Vec<u8>,
    cursor_bytes: Vec<u8>,
    chrome_pass: ChromePass,
    chrome: ChromeList,
    image_pass: ImagePass,
}

impl Renderer {
    /// Builds pipelines, atlas textures and uniforms on `gpu`.
    #[must_use]
    pub fn new(gpu: &Gpu, cfg: &RendererConfig) -> Self {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("apoth-grid"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let tex_entry =
            |binding: u32, dim: wgpu::TextureViewDimension| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: dim,
                    multisampled: false,
                },
                count: None,
            };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("apoth-grid-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1, wgpu::TextureViewDimension::D2),
                tex_entry(2, wgpu::TextureViewDimension::D2Array),
                tex_entry(3, wgpu::TextureViewDimension::D2Array),
                tex_entry(4, wgpu::TextureViewDimension::D2),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("apoth-grid-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let glyph_attrs = wgpu::vertex_attr_array![
            0 => Uint16x2, 1 => Sint16x2, 2 => Uint16x2, 3 => Uint16x2, 4 => Uint16x2,
            5 => Unorm8x4
        ];
        let glyph_buf = wgpu::VertexBufferLayout {
            array_stride: GLYPH_STRIDE,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &glyph_attrs,
        };
        let rect_attrs = wgpu::vertex_attr_array![0 => Sint32x4, 1 => Unorm8x4];
        let rect_buf = wgpu::VertexBufferLayout {
            array_stride: RECT_STRIDE,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &rect_attrs,
        };
        let over = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let pipe = |label: &str,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                    format: wgpu::TextureFormat,
                    blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let tf = cfg.target_format;
        let glyph_slot = [Some(glyph_buf)];
        let rect_slot = [Some(rect_buf)];
        let pipes = Pipes {
            bg_grid: pipe("bg-grid", "vs_quad", "fs_bg", &[], GRID_FORMAT, None),
            glyph_grid: pipe(
                "glyph-grid",
                "vs_glyph",
                "fs_glyph",
                &glyph_slot,
                GRID_FORMAT,
                over,
            ),
            rect_grid: pipe(
                "rect-grid",
                "vs_rect",
                "fs_rect",
                &rect_slot,
                GRID_FORMAT,
                over,
            ),
            composite: pipe("composite", "vs_quad", "fs_composite", &[], tf, over),
            glyph_cursor: pipe(
                "glyph-cursor",
                "vs_glyph",
                "fs_glyph",
                &glyph_slot,
                tf,
                over,
            ),
            rect_cursor: pipe("rect-cursor", "vs_rect", "fs_rect", &rect_slot, tf, over),
        };

        let atlas_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let mask_tex = texture(
            &device,
            "mask-atlas",
            (cfg.page_size, cfg.page_size, cfg.mask_pages.max(1)),
            wgpu::TextureFormat::R8Unorm,
            atlas_usage,
        );
        let color_tex = texture(
            &device,
            "color-atlas",
            (cfg.page_size, cfg.page_size, cfg.color_pages.max(1)),
            wgpu::TextureFormat::Rgba8Unorm,
            atlas_usage,
        );
        let array_view = wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        };
        let mask_view = mask_tex.create_view(&array_view);
        let color_view = color_tex.create_view(&array_view);
        let dummy = texture(
            &device,
            "dummy",
            (1, 1, 1),
            GRID_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let dummy_view = dummy.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            device,
            queue,
            cfg: cfg.clone(),
            bgl,
            pipes,
            mask_tex,
            mask_view,
            color_tex,
            color_view,
            dummy_view,
            slots: Vec::new(),
            rows_scratch: Vec::new(),
            phys_scratch: Vec::new(),
            ranges: Vec::new(),
            glyph_bytes: Vec::new(),
            rect_bytes: Vec::new(),
            bg_bytes: Vec::new(),
            cursor_bytes: Vec::new(),
            chrome_pass: ChromePass::new(gpu, cfg.target_format),
            chrome: ChromeList::new(),
            image_pass: ImagePass::new(gpu, cfg.target_format),
        }
    }

    /// The glass chrome list drawn over the grid and cursor (T-07 §4 pass 7). It is retained:
    /// a frame draws whatever it holds, so the owner clears and refills it when the interface
    /// changes and leaves it alone otherwise.
    pub fn chrome_mut(&mut self) -> &mut ChromeList {
        &mut self.chrome
    }

    /// Image textures cached on the GPU.
    #[must_use]
    pub fn image_textures(&self) -> usize {
        self.image_pass.cached()
    }

    /// Image uploads that rewrote a previous frame's texture in place (RENDERING-37).
    #[must_use]
    pub fn image_reuses(&self) -> u64 {
        self.image_pass.reused()
    }

    /// The configuration in use.
    #[must_use]
    pub fn config(&self) -> &RendererConfig {
        &self.cfg
    }

    /// New cell metrics (font size or scale changed): the retained grid is dropped and the
    /// next draw redraws every row. Atlas contents are the caller's to clear and refill.
    pub fn set_cell(&mut self, cell: (u32, u32)) {
        self.cfg.cell = cell;
        for s in &mut self.slots {
            s.targets = None;
            s.invalid = true;
        }
    }

    /// `render.text-blending` changed.
    pub fn set_text_blending(&mut self, m: TextBlending) {
        self.cfg.text_blending = m;
    }

    /// The window padding changed.
    pub fn set_origin(&mut self, origin: (i32, i32)) {
        self.cfg.origin = origin;
    }

    /// The next draw redraws every row from the scene. Call after any planned frame that was
    /// not drawn.
    pub fn invalidate_all(&mut self) {
        for s in &mut self.slots {
            s.invalid = true;
        }
    }

    /// The next draw redraws every row of the pane `key` (its scene was rebuilt from scratch).
    pub fn invalidate(&mut self, key: u64) {
        if let Some(s) = self.slots.iter_mut().find(|s| s.key == key) {
            s.invalid = true;
        }
    }

    /// Drop the retained grids of every pane key not in `keys` (closed panes).
    pub fn retain(&mut self, keys: &[u64]) {
        self.slots.retain(|s| keys.contains(&s.key));
    }

    /// Retained grids currently held.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    fn new_slot(&self, key: u64) -> Slot {
        let ubo = |label: &str| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: UNIFORM_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let vb_usage = wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST;
        Slot {
            key,
            targets: None,
            invalid: true,
            ubo_grid: ubo("ubo-grid"),
            ubo_comp: ubo("ubo-composite"),
            ubo_cursor: ubo("ubo-cursor"),
            glyph_vb: DynBuf::new(&self.device, "glyph-instances", vb_usage, 4096),
            rect_vb: DynBuf::new(&self.device, "rect-instances", vb_usage, 4096),
            cursor_rect_vb: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cursor-rects"),
                size: MAX_CURSOR_RECTS * RECT_STRIDE,
                usage: vb_usage,
                mapped_at_creation: false,
            }),
            cursor_glyph_vb: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cursor-glyph"),
                size: 64,
                usage: vb_usage,
                mapped_at_creation: false,
            }),
            cursor_rects: 0,
            cursor_glyph: false,
        }
    }

    /// Uploads an R8 coverage rect into mask page `page`.
    pub fn upload_mask(&self, page: u16, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        assert_eq!(data.len(), (w * h) as usize, "mask upload size");
        self.write_atlas(&self.mask_tex, page, (x, y), (w, h), 1, data);
    }

    /// Uploads a premultiplied RGBA rect into colour page `page`.
    pub fn upload_color(&self, page: u16, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        assert_eq!(data.len(), (w * h * 4) as usize, "colour upload size");
        self.write_atlas(&self.color_tex, page, (x, y), (w, h), 4, data);
    }

    fn write_atlas(
        &self,
        tex: &wgpu::Texture,
        page: u16,
        at: (u32, u32),
        size: (u32, u32),
        bpp: u32,
        data: &[u8],
    ) {
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: at.0,
                    y: at.1,
                    z: u32::from(page),
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.0 * bpp),
                rows_per_image: Some(size.1),
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
    }

    fn bind_group(
        &self,
        ubo: &wgpu::Buffer,
        bg: &wgpu::TextureView,
        grid: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("apoth-grid-bind"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(bg),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(grid),
                },
            ],
        })
    }

    fn make_targets(&self, slot: &mut Slot, cols: u32, rows: u32) {
        let (cw, ch) = self.cfg.cell;
        let (w, h) = (cols * cw, rows * ch);
        let usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST;
        let grid_tex = texture(
            &self.device,
            "grid",
            (w, h, 1),
            GRID_FORMAT,
            usage | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let bg_tex = texture(&self.device, "grid-bg", (cols, rows, 1), GRID_FORMAT, usage);
        let grid_view = grid_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg_view = bg_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let stride = (cols * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let stage = DynBuf::new(
            &self.device,
            "grid-bg-stage",
            wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            u64::from(stride) * u64::from(rows),
        );
        // The grid pass renders into the grid texture, so it binds a dummy in its place.
        let bind_grid = self.bind_group(&slot.ubo_grid, &bg_view, &self.dummy_view);
        let bind_comp = self.bind_group(&slot.ubo_comp, &bg_view, &grid_view);
        let bind_cursor = self.bind_group(&slot.ubo_cursor, &bg_view, &grid_view);
        slot.targets = Some(Targets {
            cols,
            rows,
            w,
            h,
            stride,
            grid_tex,
            grid_view,
            bg_tex,
            scratch: None,
            stage,
            bind_grid,
            bind_comp,
            bind_cursor,
        });
        slot.invalid = true;
    }

    /// Records the frame into `enc`: each pane's damaged rows into its retained grid, then
    /// the present pass into `target`. Nothing is submitted; the presenter owns submission.
    /// The stats are summed over the panes.
    pub fn draw(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        input: &FrameInput<'_>,
    ) -> DrawStats {
        let mut stats = DrawStats::default();
        let mut slots = std::mem::take(&mut self.slots);
        for p in input.panes {
            let i = match slots.iter().position(|s| s.key == p.key) {
                Some(i) => i,
                None => {
                    slots.push(self.new_slot(p.key));
                    slots.len() - 1
                }
            };
            let slot = &mut slots[i];
            let scene = p.scene;
            let rows = scene.rows.len() as u32;
            let cols = scene.rows.first().map_or(0, |r| r.bg.len()) as u32;
            if rows == 0 || cols == 0 {
                continue;
            }
            if slot
                .targets
                .as_ref()
                .is_none_or(|t| t.cols != cols || t.rows != rows)
            {
                self.make_targets(slot, cols, rows);
            }
            if let Some(mut t) = slot.targets.take() {
                let st = self.grid_pass(enc, slot, &mut t, scene);
                slot.targets = Some(t);
                stats.rows_drawn += st.rows_drawn;
                stats.glyphs += st.glyphs;
                stats.rects += st.rects;
                stats.region_copies += st.region_copies;
                stats.full |= st.full;
            }
        }
        self.present_pass(enc, target, target_size, input, &mut slots);
        self.slots = slots;
        self.chrome_pass
            .record(enc, target, target_size, &self.chrome);
        stats
    }

    fn grid_pass(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        slot: &mut Slot,
        t: &mut Targets,
        scene: &GridScene,
    ) -> DrawStats {
        let (cw, ch) = self.cfg.cell;
        let ring = scene.ring;
        let full = std::mem::take(&mut slot.invalid);
        let mut stats = DrawStats {
            full,
            ..DrawStats::default()
        };

        // Scroll operations first: they read rows that the redraw below may overwrite.
        if !full {
            debug_assert!(scene.plan().ops.len() <= 1, "one scroll op per frame");
            for op in &scene.plan().ops {
                if let Op::RegionCopy { top, bottom, delta } = *op {
                    self.region_copy(enc, t, &ring, (top, bottom), delta);
                    stats.region_copies += 1;
                }
            }
        }

        self.rows_scratch.clear();
        if full {
            self.rows_scratch.extend(0..t.rows);
        } else {
            self.rows_scratch.extend_from_slice(scene.redrawn());
        }
        if self.rows_scratch.is_empty() {
            return stats;
        }

        // Pack instances and stage background rows, in redraw order.
        self.glyph_bytes.clear();
        self.rect_bytes.clear();
        self.ranges.clear();
        self.phys_scratch.clear();
        let (mut gn, mut rn) = (0u32, 0u32);
        for &y in &self.rows_scratch {
            let row = &scene.rows[y as usize];
            let phys = ring.phys(y as usize) as u32;
            self.bg_bytes.clear();
            for px in &row.bg {
                self.bg_bytes.extend_from_slice(px);
            }
            self.queue.write_buffer(
                &t.stage.buf,
                u64::from(phys) * u64::from(t.stride),
                &self.bg_bytes,
            );
            let (g0, r0) = (gn, rn);
            for g in &row.glyphs {
                pack_glyph(g, &mut self.glyph_bytes);
                gn += 1;
            }
            for r in &row.rects {
                pack_rect(r, phys as i32 * ch as i32, &mut self.rect_bytes);
                rn += 1;
            }
            self.ranges.push(RowRange {
                phys,
                g0,
                g1: gn,
                r0,
                r1: rn,
            });
            self.phys_scratch.push(phys);
        }
        self.phys_scratch.sort_unstable();
        stats.rows_drawn = self.rows_scratch.len();
        stats.glyphs = gn as usize;
        stats.rects = rn as usize;

        if !self.glyph_bytes.is_empty() {
            slot.glyph_vb
                .ensure(&self.device, self.glyph_bytes.len() as u64);
            self.queue
                .write_buffer(&slot.glyph_vb.buf, 0, &self.glyph_bytes);
        }
        if !self.rect_bytes.is_empty() {
            slot.rect_vb
                .ensure(&self.device, self.rect_bytes.len() as u64);
            self.queue
                .write_buffer(&slot.rect_vb.buf, 0, &self.rect_bytes);
        }
        let uni = Uni {
            target: (t.w, t.h),
            cell: (cw, ch),
            origin: (0, 0),
            ring: (ring.offset() as u32, ring.rows() as u32),
            grid: (t.cols, t.rows),
            mode: mode_of(self.cfg.text_blending),
            flags: 0,
            cursor_dst: [0.0; 4],
        };
        self.queue.write_buffer(&slot.ubo_grid, 0, &uni.bytes());

        // Staged background rows into the background ring, one copy per physical run.
        let mut i = 0;
        let mut bg_runs: Vec<(u32, u32)> = Vec::new();
        while i < self.phys_scratch.len() {
            let start = self.phys_scratch[i];
            let mut n = 1;
            while i + (n as usize) < self.phys_scratch.len()
                && self.phys_scratch[i + n as usize] == start + n
            {
                n += 1;
            }
            bg_runs.push((start, n));
            i += n as usize;
        }
        for &(start, n) in &bg_runs {
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &t.stage.buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: u64::from(start) * u64::from(t.stride),
                        bytes_per_row: Some(t.stride),
                        rows_per_image: None,
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &t.bg_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: start,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: t.cols,
                    height: n,
                    depth_or_array_layers: 1,
                },
            );
        }

        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grid"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.grid_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_bind_group(0, &t.bind_grid, &[]);
        pass.set_pipeline(&self.pipes.bg_grid);
        for &(start, n) in &bg_runs {
            pass.set_scissor_rect(0, start * ch, t.w, n * ch);
            pass.draw(0..4, 0..1);
        }
        if gn > 0 {
            pass.set_pipeline(&self.pipes.glyph_grid);
            pass.set_vertex_buffer(0, slot.glyph_vb.buf.slice(..));
            for r in &self.ranges {
                if r.g1 > r.g0 {
                    pass.set_scissor_rect(0, r.phys * ch, t.w, ch);
                    pass.draw(0..4, r.g0..r.g1);
                }
            }
        }
        if rn > 0 {
            pass.set_pipeline(&self.pipes.rect_grid);
            pass.set_vertex_buffer(0, slot.rect_vb.buf.slice(..));
            for r in &self.ranges {
                if r.r1 > r.r0 {
                    pass.set_scissor_rect(0, r.phys * ch, t.w, ch);
                    pass.draw(0..4, r.r0..r.r1);
                }
            }
        }
        drop(pass);
        stats
    }

    /// Region scroll through scratch textures: stage the source rows, then write them back at
    /// their destination, so the two ranges may overlap.
    fn region_copy(
        &self,
        enc: &mut wgpu::CommandEncoder,
        t: &mut Targets,
        ring: &RowRing,
        span: (usize, usize),
        delta: i32,
    ) {
        let (top, bottom) = span;
        let n = delta.unsigned_abs() as usize;
        let count = (bottom + 1 - top).saturating_sub(n);
        if count == 0 {
            return;
        }
        let (src_first, dst_first) = if delta > 0 {
            (top + n, top)
        } else {
            (top, top + n)
        };
        if t.scratch.is_none() {
            let usage = wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
            t.scratch = Some((
                texture(
                    &self.device,
                    "scratch-grid",
                    (t.w, t.h, 1),
                    GRID_FORMAT,
                    usage,
                ),
                texture(
                    &self.device,
                    "scratch-bg",
                    (t.cols, t.rows, 1),
                    GRID_FORMAT,
                    usage,
                ),
            ));
        }
        let Some((sg, sb)) = t.scratch.as_ref() else {
            return;
        };
        let ch = self.cfg.cell.1;
        let mut k = 0u32;
        for (p, len) in runs(ring, src_first, count) {
            if len == 0 {
                continue;
            }
            let (p, len) = (p as u32, len as u32);
            copy_tex(
                enc,
                &t.grid_tex,
                (0, p * ch),
                sg,
                (0, k * ch),
                (t.w, len * ch),
            );
            copy_tex(enc, &t.bg_tex, (0, p), sb, (0, k), (t.cols, len));
            k += len;
        }
        k = 0;
        for (p, len) in runs(ring, dst_first, count) {
            if len == 0 {
                continue;
            }
            let (p, len) = (p as u32, len as u32);
            copy_tex(
                enc,
                sg,
                (0, k * ch),
                &t.grid_tex,
                (0, p * ch),
                (t.w, len * ch),
            );
            copy_tex(enc, sb, (0, k), &t.bg_tex, (0, p), (t.cols, len));
            k += len;
        }
    }

    fn present_pass(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        input: &FrameInput<'_>,
        slots: &mut [Slot],
    ) {
        let (cw, ch) = self.cfg.cell;
        let mode = mode_of(self.cfg.text_blending);
        // Uniforms and cursor instances first: every write lands before the submission.
        for p in input.panes {
            let Some(slot) = slots.iter_mut().find(|s| s.key == p.key) else {
                continue;
            };
            slot.cursor_rects = 0;
            slot.cursor_glyph = false;
            let Some(t) = slot.targets.as_ref() else {
                continue;
            };
            let ring = p.scene.ring;
            let base = Uni {
                target: target_size,
                cell: (cw, ch),
                origin: p.origin.unwrap_or(self.cfg.origin),
                ring: (ring.offset() as u32, ring.rows() as u32),
                grid: (t.cols, t.rows),
                mode,
                flags: 0,
                cursor_dst: [0.0; 4],
            };
            self.queue.write_buffer(&slot.ubo_comp, 0, &base.bytes());
            let mut cursor_uni = Uni {
                ring: (0, 0),
                ..base
            };
            if let Some(c) = p.cursor {
                self.cursor_bytes.clear();
                for r in c.rects.iter().take(MAX_CURSOR_RECTS as usize) {
                    pack_rect(r, 0, &mut self.cursor_bytes);
                    slot.cursor_rects += 1;
                }
                if slot.cursor_rects > 0 {
                    self.queue
                        .write_buffer(&slot.cursor_rect_vb, 0, &self.cursor_bytes);
                }
                if let Some(g) = &c.glyph {
                    self.cursor_bytes.clear();
                    pack_glyph(g, &mut self.cursor_bytes);
                    self.queue
                        .write_buffer(&slot.cursor_glyph_vb, 0, &self.cursor_bytes);
                    slot.cursor_glyph = true;
                    cursor_uni.flags = 1;
                    if let Some(r) = c.rects.first() {
                        cursor_uni.cursor_dst = r.color.map(|b| f32::from(b) / 255.0);
                    }
                }
            }
            self.queue
                .write_buffer(&slot.ubo_cursor, 0, &cursor_uni.bytes());
        }

        // Image quads and textures, per pane in input order (slot-less panes draw nothing).
        let mut img_panes = Vec::with_capacity(input.panes.len());
        for p in input.panes {
            let grid = slots
                .iter()
                .find(|s| s.key == p.key)
                .and_then(|s| s.targets.as_ref())
                .map_or((0, 0), |t| (t.cols * cw, t.rows * ch));
            img_panes.push((
                p.origin.unwrap_or(self.cfg.origin),
                grid,
                p.backdrop,
                p.images_below,
                p.images_above,
            ));
        }
        self.image_pass.prepare(target_size, &img_panes);
        let full = (0, 0, target_size.0, target_size.1);
        let clip = |o: (i32, i32), size: (u32, u32)| {
            let x0 = o.0.clamp(0, target_size.0 as i32) as u32;
            let y0 = o.1.clamp(0, target_size.1 as i32) as u32;
            let x1 = (o.0 + size.0 as i32).clamp(0, target_size.0 as i32) as u32;
            let y1 = (o.1 + size.1 as i32).clamp(0, target_size.1 as i32) as u32;
            (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
        };

        let [r, g, b, a] = input.clear;
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("present"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(r),
                        g: f64::from(g),
                        b: f64::from(b),
                        a: f64::from(a),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        for (i, p) in input.panes.iter().enumerate() {
            let Some(slot) = slots.iter().find(|s| s.key == p.key) else {
                continue;
            };
            let Some(t) = slot.targets.as_ref() else {
                continue;
            };
            let (below, above) = (
                self.image_pass.has(i, Band::Below),
                self.image_pass.has(i, Band::Above),
            );
            let c = clip(img_panes[i].0, img_panes[i].1);
            let visible = c.2 > 0 && c.3 > 0;
            if below && visible {
                pass.set_scissor_rect(c.0, c.1, c.2, c.3);
                self.image_pass.draw(&mut pass, i, Band::Below);
                pass.set_scissor_rect(full.0, full.1, full.2, full.3);
            }
            pass.set_pipeline(&self.pipes.composite);
            pass.set_bind_group(0, &t.bind_comp, &[]);
            pass.draw(0..4, 0..1);
            if above && visible {
                pass.set_scissor_rect(c.0, c.1, c.2, c.3);
                self.image_pass.draw(&mut pass, i, Band::Above);
                pass.set_scissor_rect(full.0, full.1, full.2, full.3);
            }
            if slot.cursor_rects > 0 {
                pass.set_pipeline(&self.pipes.rect_cursor);
                pass.set_bind_group(0, &t.bind_cursor, &[]);
                pass.set_vertex_buffer(0, slot.cursor_rect_vb.slice(..));
                pass.draw(0..4, 0..slot.cursor_rects);
            }
            if slot.cursor_glyph {
                pass.set_pipeline(&self.pipes.glyph_cursor);
                pass.set_bind_group(0, &t.bind_cursor, &[]);
                pass.set_vertex_buffer(0, slot.cursor_glyph_vb.slice(..));
                pass.draw(0..4, 0..1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_block_is_64_bytes_in_shader_order() {
        let u = Uni {
            target: (640, 480),
            cell: (8, 16),
            origin: (-2, 3),
            ring: (5, 30),
            grid: (80, 30),
            mode: 2,
            flags: 1,
            cursor_dst: [1.0, 0.5, 0.0, 1.0],
        };
        let b = u.bytes();
        assert_eq!(b.len(), 64);
        assert_eq!(f32::from_le_bytes(b[0..4].try_into().unwrap()), 640.0);
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 8);
        assert_eq!(i32::from_le_bytes(b[16..20].try_into().unwrap()), -2);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 5);
        assert_eq!(u32::from_le_bytes(b[32..36].try_into().unwrap()), 80);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 1);
        assert_eq!(f32::from_le_bytes(b[52..56].try_into().unwrap()), 0.5);
    }

    #[test]
    fn glyph_instance_packs_to_the_24_byte_layout() {
        let g = GlyphInstance {
            cell: [3, 4],
            offset: [-1, 2],
            atlas_xy: [10, 20],
            size: [6, 10],
            page: 1,
            flags: 4,
            color: [9, 8, 7, 255],
        };
        let mut v = Vec::new();
        pack_glyph(&g, &mut v);
        assert_eq!(v.len() as u64, GLYPH_STRIDE);
        assert_eq!(&v[0..4], &[3, 0, 4, 0]);
        assert_eq!(&v[4..8], &[0xff, 0xff, 2, 0]);
        assert_eq!(&v[16..20], &[1, 0, 4, 0]);
        assert_eq!(&v[20..24], &[9, 8, 7, 255]);
    }

    #[test]
    fn rect_instance_packs_to_20_bytes_with_row_offset() {
        let r = RectInstance::new(1, 2, 3, 4, [5, 6, 7, 8]);
        let mut v = Vec::new();
        pack_rect(&r, 32, &mut v);
        assert_eq!(v.len() as u64, RECT_STRIDE);
        assert_eq!(i32::from_le_bytes(v[4..8].try_into().unwrap()), 34);
        assert_eq!(&v[16..20], &[5, 6, 7, 8]);
    }

    #[test]
    fn ring_runs_split_at_the_wrap() {
        let mut ring = RowRing::new(10);
        assert_eq!(runs(&ring, 2, 4), [(2, 4), (0, 0)]);
        ring.advance(7);
        // logical 0..4 -> physical 7, 8, 9, 0
        assert_eq!(runs(&ring, 0, 4), [(7, 3), (0, 1)]);
        assert_eq!(runs(&ring, 0, 0), [(0, 0), (0, 0)]);
    }
}
