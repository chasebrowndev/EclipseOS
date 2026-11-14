// SPDX-License-Identifier: AGPL-3.0-only
//! The glass chrome pass (T-08 §3-§4, T-07 §4 pass 7): rounded rects with a crisp rim and
//! translucent fill, plus the analytic blur-off shadow, drawn over the grid.
//!
//! `apoth-ui` emits a draw list (`RoundedRect`, `Shadow`, `Clip`); this module does not
//! depend on that crate. The binary adapts the list to [`ChromeSink`] (one call per item), so
//! the two crates agree only on this small trait. Text and icons take the glyph path (T-06)
//! and are not drawn here.
//!
//! Colours enter as straight-alpha RGBA8 (the token table) and are premultiplied once, here;
//! the pipeline blends `One, OneMinusSrcAlpha`, so a translucent fill leaves the window's alpha
//! below 1 and the compositor's `ext_background_effect` blur shows through. There is no
//! blur, glow, bloom or gradient primitive: a rect is one fill and one hairline rim.
//!
//! Layers: [`ChromeList`] (GPU free, retained between frames) -> [`ChromePass`] (wgpu) and
//! [`reference`] (CPU renderer mirroring `shaders/chrome.wgsl`, the golden source).

use crate::gpu::Gpu;
use crate::reference::Image;

const SHADER: &str = include_str!("shaders/chrome.wgsl");

/// Bytes of one packed instance.
pub const INSTANCE_BYTES: usize = 72;
const UNIFORM_BYTES: u64 = 16;
const KIND_RECT: f32 = 0.0;
const KIND_SHADOW: f32 = 1.0;
/// A flat axis-aligned rect: premultiplied fill, no edge antialiasing. The glyph bridge
/// (T-08 §2, text via the T-06 path) emits one per run of equal coverage in a glyph row.
const KIND_FLAT: f32 = 2.0;
const UNBOUNDED: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

/// A pixel rectangle: x, y, width, height (target pixels, y down).
pub type Rect = [f32; 4];

/// One rounded rect: translucent fill and a hairline rim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeRect {
    /// x, y, w, h.
    pub rect: Rect,
    /// Corner radii: top-left, top-right, bottom-right, bottom-left.
    pub radii: [f32; 4],
    /// Straight-alpha RGBA8 fill.
    pub fill: [u8; 4],
    /// Straight-alpha RGBA8 rim colour.
    pub rim: [u8; 4],
    /// Rim width in pixels; 0 draws no rim.
    pub rim_width: f32,
}

/// The analytic shadow of a rect (no blur pass): black at `alpha` at the edge, falling off
/// over `spread` pixels outside it, nothing under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeShadow {
    /// The casting rect, already offset by the caller.
    pub rect: Rect,
    /// Corner radii as for [`ChromeRect`].
    pub radii: [f32; 4],
    /// Falloff distance in pixels.
    pub spread: f32,
    /// Alpha at the rect's edge, 0..=1.
    pub alpha: f32,
}

/// What a draw-list producer is adapted to. Calls arrive in paint order.
pub trait ChromeSink {
    /// A rounded rect with fill and rim.
    fn rounded_rect(&mut self, r: &ChromeRect);
    /// A shadow beneath the following rects.
    fn shadow(&mut self, s: &ChromeShadow);
    /// Restricts later items to `rect` (intersected with the enclosing clip).
    fn push_clip(&mut self, rect: Rect);
    /// Ends the innermost clip.
    fn pop_clip(&mut self);
    /// An unantialiased rect of one straight-alpha colour: glyph coverage from the text
    /// bridge.
    fn flat(&mut self, rect: Rect, colour: [u8; 4]);
    /// Ends a layer: the items so far draw under the next floating pane (see
    /// [`ChromeList::marks`]). Sinks without layers ignore it.
    fn layer(&mut self) {}
}

/// Straight to premultiplied RGBA8.
#[must_use]
pub fn premultiply(c: [u8; 4]) -> [u8; 4] {
    let a = u32::from(c[3]);
    let m = |v: u8| ((u32::from(v) * a + 127) / 255) as u8;
    [m(c[0]), m(c[1]), m(c[2]), c[3]]
}

/// A packed instance, in shader order. Public so the binary can size and inspect lists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeInstance {
    /// x, y, w, h.
    pub rect: Rect,
    /// Radii, already clamped to half the shorter side.
    pub radii: [f32; 4],
    /// Clip x0, y0, x1, y1.
    pub clip: [f32; 4],
    /// Rim width, shadow spread, kind, unused.
    pub params: [f32; 4],
    /// Premultiplied fill.
    pub fill: [u8; 4],
    /// Premultiplied rim.
    pub rim: [u8; 4],
}

impl ChromeInstance {
    fn is_shadow(&self) -> bool {
        self.params[2] > 0.5 && self.params[2] < 1.5
    }

    fn is_flat(&self) -> bool {
        self.params[2] >= 1.5
    }

    /// Appends the 72 packed bytes (little endian).
    pub fn pack(&self, out: &mut Vec<u8>) {
        for v in self
            .rect
            .iter()
            .chain(&self.radii)
            .chain(&self.clip)
            .chain(&self.params)
        {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.fill);
        out.extend_from_slice(&self.rim);
    }
}

fn clamp_radii(rect: Rect, radii: [f32; 4]) -> [f32; 4] {
    let max = (rect[2].min(rect[3]) * 0.5).max(0.0);
    radii.map(|r| r.clamp(0.0, max))
}

/// The retained chrome draw list. Reuse one across frames: `clear` keeps capacity.
#[derive(Clone, Debug, Default)]
pub struct ChromeList {
    /// Instances in paint order.
    pub instances: Vec<ChromeInstance>,
    clips: Vec<[f32; 4]>,
    /// Layer ends, as instance counts: layer `k` is `marks[k-1]..marks[k]` (from 0), the
    /// last layer runs to the end. The renderer draws layer `k < marks.len()` right before
    /// the pane [`crate::renderer::FrameInput::chrome_before`] names, the last after every
    /// pane.
    pub marks: Vec<u32>,
}

impl ChromeList {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops all items and clips (capacity kept).
    pub fn clear(&mut self) {
        self.instances.clear();
        self.clips.clear();
        self.marks.clear();
    }

    /// Instance range of layer `k` (see [`ChromeList::marks`]).
    #[must_use]
    pub fn layer_range(&self, k: usize) -> std::ops::Range<u32> {
        let start = if k == 0 {
            0
        } else {
            self.marks.get(k - 1).copied().unwrap_or(0)
        };
        let end = self
            .marks
            .get(k)
            .copied()
            .unwrap_or(self.instances.len() as u32);
        start.min(end)..end
    }

    /// No items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    fn clip(&self) -> [f32; 4] {
        self.clips.last().copied().unwrap_or(UNBOUNDED)
    }
}

impl ChromeSink for ChromeList {
    fn rounded_rect(&mut self, r: &ChromeRect) {
        if r.rect[2] <= 0.0 || r.rect[3] <= 0.0 {
            return;
        }
        self.instances.push(ChromeInstance {
            rect: r.rect,
            radii: clamp_radii(r.rect, r.radii),
            clip: self.clip(),
            params: [r.rim_width.max(0.0), 0.0, KIND_RECT, 0.0],
            fill: premultiply(r.fill),
            rim: premultiply(r.rim),
        });
    }

    fn shadow(&mut self, s: &ChromeShadow) {
        let a = (s.alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        if s.rect[2] <= 0.0 || s.rect[3] <= 0.0 || a == 0 {
            return;
        }
        self.instances.push(ChromeInstance {
            rect: s.rect,
            radii: clamp_radii(s.rect, s.radii),
            clip: self.clip(),
            // The smoothstep needs a non-empty range.
            params: [0.0, s.spread.max(1.0), KIND_SHADOW, 0.0],
            fill: [0, 0, 0, a],
            rim: [0; 4],
        });
    }

    fn flat(&mut self, rect: Rect, colour: [u8; 4]) {
        if rect[2] <= 0.0 || rect[3] <= 0.0 || colour[3] == 0 {
            return;
        }
        self.instances.push(ChromeInstance {
            rect,
            radii: [0.0; 4],
            clip: self.clip(),
            params: [0.0, 0.0, KIND_FLAT, 0.0],
            fill: premultiply(colour),
            rim: [0; 4],
        });
    }

    fn push_clip(&mut self, rect: Rect) {
        let cur = self.clip();
        let c = [
            rect[0].max(cur[0]),
            rect[1].max(cur[1]),
            (rect[0] + rect[2]).min(cur[2]),
            (rect[1] + rect[3]).min(cur[3]),
        ];
        self.clips.push(c);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn layer(&mut self) {
        self.marks.push(self.instances.len() as u32);
    }
}

// ---- CPU reference (mirrors shaders/chrome.wgsl) -----------------------------------------

/// CPU renderer of a [`ChromeList`] into a premultiplied [`Image`]; the golden source and the
/// oracle the GPU pass is compared with.
pub mod reference {
    use super::{ChromeInstance, ChromeList, Image};

    fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn sd_round(p: [f32; 2], half: [f32; 2], r: [f32; 4]) -> f32 {
        let right = p[0] > 0.0;
        let bottom = p[1] > 0.0;
        let rad = match (right, bottom) {
            (true, false) => r[1],
            (true, true) => r[2],
            (false, false) => r[0],
            (false, true) => r[3],
        };
        let q = [p[0].abs() - half[0] + rad, p[1].abs() - half[1] + rad];
        let outside = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
        q[0].max(q[1]).min(0.0) + outside - rad
    }

    fn unorm(c: [u8; 4]) -> [f32; 4] {
        c.map(|v| f32::from(v) / 255.0)
    }

    /// Premultiplied colour of instance `i` at pixel centre `(fx, fy)`; zero when clipped.
    fn shade(i: &ChromeInstance, fx: f32, fy: f32) -> [f32; 4] {
        if fx < i.clip[0] || fy < i.clip[1] || fx >= i.clip[2] || fy >= i.clip[3] {
            return [0.0; 4];
        }
        if i.is_flat() {
            let inside = fx >= i.rect[0]
                && fy >= i.rect[1]
                && fx < i.rect[0] + i.rect[2]
                && fy < i.rect[1] + i.rect[3];
            return if inside { unorm(i.fill) } else { [0.0; 4] };
        }
        let half = [i.rect[2] * 0.5, i.rect[3] * 0.5];
        let c = [i.rect[0] + half[0], i.rect[1] + half[1]];
        let d = sd_round([fx - c[0], fy - c[1]], half, i.radii);
        let fill = unorm(i.fill);
        if i.is_shadow() {
            let fall = 1.0 - smoothstep(0.0, i.params[1], d);
            let outside = (d + 0.5).clamp(0.0, 1.0);
            return fill.map(|v| v * fall * outside);
        }
        let rim = unorm(i.rim);
        let cov = (0.5 - d).clamp(0.0, 1.0);
        let t = if i.params[0] <= 0.0 {
            0.0
        } else {
            (d + i.params[0] + 0.5).clamp(0.0, 1.0)
        };
        std::array::from_fn(|k| {
            let over = rim[k] + fill[k] * (1.0 - rim[3]);
            (fill[k] + (over - fill[k]) * t) * cov
        })
    }

    /// Draws `list` over `img` with premultiplied `One, OneMinusSrcAlpha` compositing.
    pub fn render(list: &ChromeList, img: &mut Image) {
        for i in &list.instances {
            let margin = if i.is_shadow() {
                i.params[1] + 1.0
            } else if i.is_flat() {
                0.0
            } else {
                1.0
            };
            let (x0, y0) = (i.rect[0] - margin, i.rect[1] - margin);
            let (x1, y1) = (
                i.rect[0] + i.rect[2] + margin,
                i.rect[1] + i.rect[3] + margin,
            );
            for py in (y0.floor() as i32).max(0)..(y1.ceil() as i32).min(img.h as i32) {
                for px in (x0.floor() as i32).max(0)..(x1.ceil() as i32).min(img.w as i32) {
                    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                    if fx < x0 || fy < y0 || fx > x1 || fy > y1 {
                        continue;
                    }
                    let c = shade(i, fx, fy);
                    let q = c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                    if q[3] != 0 || q[0] != 0 || q[1] != 0 || q[2] != 0 {
                        img.over(px, py, q);
                    }
                }
            }
        }
    }
}

// ---- GPU pass ----------------------------------------------------------------------------

/// The wgpu pipeline for a [`ChromeList`]. Draws into an existing target with `LoadOp::Load`.
#[derive(Debug)]
pub struct ChromePass {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    ubo: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vb: wgpu::Buffer,
    cap: u64,
    bytes: Vec<u8>,
}

const VB_USAGE: wgpu::BufferUsages = wgpu::BufferUsages::VERTEX.union(wgpu::BufferUsages::COPY_DST);

fn vertex_buffer(device: &wgpu::Device, cap: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("chrome-instances"),
        size: cap,
        usage: VB_USAGE,
        mapped_at_creation: false,
    })
}

impl ChromePass {
    /// Builds the pipeline for a target of `format`.
    #[must_use]
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("apoth-chrome"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("apoth-chrome-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("apoth-chrome-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
            4 => Unorm8x4, 5 => Unorm8x4
        ];
        let buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: INSTANCE_BYTES as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &attrs,
        })];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chrome"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_chrome"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_chrome"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chrome-ubo"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("apoth-chrome-bind"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        let cap = 64 * INSTANCE_BYTES as u64;
        Self {
            vb: vertex_buffer(&device, cap),
            device,
            queue,
            pipeline,
            ubo,
            bind,
            cap,
            bytes: Vec::new(),
        }
    }

    /// Uploads the list's instances and the target size for [`ChromePass::draw_range`]
    /// inside a pass the caller owns. False for an empty list (nothing to draw).
    pub fn upload(&mut self, target_size: (u32, u32), list: &ChromeList) -> bool {
        if list.is_empty() {
            return false;
        }
        self.bytes.clear();
        for i in &list.instances {
            i.pack(&mut self.bytes);
        }
        let need = self.bytes.len() as u64;
        if need > self.cap {
            self.cap = need.next_power_of_two();
            self.vb = vertex_buffer(&self.device, self.cap);
        }
        self.queue.write_buffer(&self.vb, 0, &self.bytes);
        let mut u = [0u8; UNIFORM_BYTES as usize];
        u[0..4].copy_from_slice(&(target_size.0 as f32).to_le_bytes());
        u[4..8].copy_from_slice(&(target_size.1 as f32).to_le_bytes());
        self.queue.write_buffer(&self.ubo, 0, &u);
        true
    }

    /// Draws instances `range` of the last [`ChromePass::upload`] into `pass`.
    pub fn draw_range(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        let need = self.bytes.len() as u64;
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..need));
        pass.draw(0..4, range);
    }

    /// Records the list into `enc` over `target` (existing contents kept). Does nothing for an
    /// empty list.
    pub fn record(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        list: &ChromeList,
    ) {
        if list.is_empty() {
            return;
        }
        self.bytes.clear();
        for i in &list.instances {
            i.pack(&mut self.bytes);
        }
        let need = self.bytes.len() as u64;
        if need > self.cap {
            self.cap = need.next_power_of_two();
            self.vb = vertex_buffer(&self.device, self.cap);
        }
        self.queue.write_buffer(&self.vb, 0, &self.bytes);
        let mut u = [0u8; UNIFORM_BYTES as usize];
        u[0..4].copy_from_slice(&(target_size.0 as f32).to_le_bytes());
        u[4..8].copy_from_slice(&(target_size.1 as f32).to_le_bytes());
        self.queue.write_buffer(&self.ubo, 0, &u);

        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("chrome"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..need));
        pass.draw(0..4, 0..list.instances.len() as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> ChromeRect {
        ChromeRect {
            rect: [x, y, w, h],
            radii: [4.0; 4],
            fill: [255, 255, 255, 128],
            rim: [255, 255, 255, 64],
            rim_width: 1.0,
        }
    }

    #[test]
    fn colours_are_premultiplied_once() {
        assert_eq!(premultiply([255, 255, 255, 128]), [128, 128, 128, 128]);
        assert_eq!(premultiply([200, 100, 0, 0]), [0, 0, 0, 0]);
        assert_eq!(premultiply([10, 20, 30, 255]), [10, 20, 30, 255]);
        let mut l = ChromeList::new();
        l.rounded_rect(&rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(l.instances[0].fill, [128, 128, 128, 128]);
        assert_eq!(l.instances[0].rim, [64, 64, 64, 64]);
    }

    #[test]
    fn radii_clamp_to_half_the_short_side() {
        let mut l = ChromeList::new();
        let mut r = rect(0.0, 0.0, 40.0, 10.0);
        r.radii = [100.0, 3.0, -2.0, 5.0];
        l.rounded_rect(&r);
        assert_eq!(l.instances[0].radii, [5.0, 3.0, 0.0, 5.0]);
    }

    #[test]
    fn clips_nest_by_intersection() {
        let mut l = ChromeList::new();
        l.push_clip([0.0, 0.0, 100.0, 100.0]);
        l.push_clip([50.0, 20.0, 100.0, 100.0]);
        l.rounded_rect(&rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(l.instances[0].clip, [50.0, 20.0, 100.0, 100.0]);
        l.pop_clip();
        l.rounded_rect(&rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(l.instances[1].clip, [0.0, 0.0, 100.0, 100.0]);
        l.pop_clip();
        l.rounded_rect(&rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(l.instances[2].clip, UNBOUNDED);
    }

    #[test]
    fn empty_rects_and_invisible_shadows_are_dropped() {
        let mut l = ChromeList::new();
        l.rounded_rect(&rect(0.0, 0.0, 0.0, 10.0));
        l.shadow(&ChromeShadow {
            rect: [0.0, 0.0, 10.0, 10.0],
            radii: [0.0; 4],
            spread: 8.0,
            alpha: 0.0,
        });
        assert!(l.is_empty());
    }

    #[test]
    fn packed_instance_is_72_bytes_in_shader_order() {
        let mut l = ChromeList::new();
        l.rounded_rect(&rect(1.0, 2.0, 30.0, 20.0));
        let mut b = Vec::new();
        l.instances[0].pack(&mut b);
        assert_eq!(b.len(), INSTANCE_BYTES);
        assert_eq!(f32::from_le_bytes(b[0..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(b[8..12].try_into().unwrap()), 30.0);
        // params.x (rim width) sits at 48 + 0 after rect, radii, clip.
        assert_eq!(f32::from_le_bytes(b[48..52].try_into().unwrap()), 1.0);
        assert_eq!(&b[64..68], &[128; 4]);
        assert_eq!(&b[68..72], &[64; 4]);
    }

    #[test]
    fn clear_keeps_capacity() {
        let mut l = ChromeList::new();
        for _ in 0..50 {
            l.rounded_rect(&rect(0.0, 0.0, 10.0, 10.0));
        }
        let cap = l.instances.capacity();
        l.clear();
        assert!(l.is_empty());
        assert_eq!(l.instances.capacity(), cap);
    }
}
