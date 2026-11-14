// SPDX-License-Identifier: AGPL-3.0-only
//! The image pipeline (T-15 §6; T-07 §4 passes 2 and 4; RENDERING-35..37): one textured
//! quad per placement, drawn under the grid (negative z) or over it, clipped to the pane.
//!
//! Textures are cached by the caller's key (a process-unique blob serial, so equal keys are
//! equal pixels): uploaded once, premultiplied on the way, downscaled when an axis exceeds
//! the device limit, and dropped after [`IDLE_FRAMES`] frames unused or when the cache
//! passes [`CACHE_BYTES`]. A 1:1 quad samples nearest (pixel exact), a scaled one linear.

use std::collections::HashMap;

use crate::gpu::Gpu;

const SHADER: &str = include_str!("shaders/image.wgsl");
/// Bytes of one packed instance: dst, uv, tint (three `vec4<f32>`).
pub const INSTANCE_BYTES: usize = 48;
/// Frames a texture survives unused.
pub const IDLE_FRAMES: u64 = 240;
/// Cache budget; past it, textures unused this frame go first.
pub const CACHE_BYTES: u64 = 512 << 20;

/// One placement to draw.
#[derive(Clone, Copy, Debug)]
pub struct ImageQuad<'a> {
    /// Texture key: equal keys must mean equal pixels.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8, `width * height * 4` bytes (read only on first upload).
    pub rgba: &'a [u8],
    /// x, y, w, h in pixels relative to the pane's grid origin.
    pub dst: [i32; 4],
    /// Source rect in image pixels: x, y, w, h.
    pub src: [u32; 4],
}

#[derive(Debug)]
struct Tex {
    _tex: wgpu::Texture,
    nearest: wgpu::BindGroup,
    linear: wgpu::BindGroup,
    bytes: u64,
    used: u64,
}

/// Which band of a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Band {
    Below,
    Above,
}

#[derive(Clone, Debug, Default)]
struct PaneRanges {
    /// (instance index, texture key or 0 for the backdrop, nearest)
    below: Vec<(u32, u64, bool)>,
    above: Vec<(u32, u64, bool)>,
}

/// GPU side of image drawing; owned by the renderer.
#[derive(Debug)]
pub(crate) struct ImagePass {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    tex_bgl: wgpu::BindGroupLayout,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    ubo: wgpu::Buffer,
    bind: wgpu::BindGroup,
    white: Tex,
    vb: wgpu::Buffer,
    cap: u64,
    bytes: Vec<u8>,
    cache: HashMap<u64, Tex>,
    frame: u64,
    panes: Vec<PaneRanges>,
    max_dim: u32,
}

fn vertex_buffer(device: &wgpu::Device, cap: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image-instances"),
        size: cap,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Straight to premultiplied, nearest-downscaled to fit `max` per axis.
fn prepare_pixels(w: u32, h: u32, rgba: &[u8], max: u32) -> (u32, u32, Vec<u8>) {
    let (tw, th) = if w > max || h > max {
        let s = f64::from(max) / f64::from(w.max(h));
        (
            ((f64::from(w) * s) as u32).clamp(1, max),
            ((f64::from(h) * s) as u32).clamp(1, max),
        )
    } else {
        (w, h)
    };
    let mut out = Vec::with_capacity(tw as usize * th as usize * 4);
    for y in 0..th {
        let sy = (u64::from(y) * u64::from(h) / u64::from(th)) as usize;
        for x in 0..tw {
            let sx = (u64::from(x) * u64::from(w) / u64::from(tw)) as usize;
            let i = (sy * w as usize + sx) * 4;
            let px = rgba.get(i..i + 4).unwrap_or(&[0, 0, 0, 0]);
            let a = u32::from(px[3]);
            let pm = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
            out.extend_from_slice(&[pm(px[0]), pm(px[1]), pm(px[2]), px[3]]);
        }
    }
    (tw, th, out)
}

impl ImagePass {
    pub(crate) fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("apoth-image"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let ubo_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("apoth-image-ubo-bgl"),
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
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("apoth-image-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("apoth-image-layout"),
            bind_group_layouts: &[Some(&ubo_bgl), Some(&tex_bgl)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
        let buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: INSTANCE_BYTES as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &attrs,
        })];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_image"),
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
                entry_point: Some("fs_image"),
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
        let sampler = |filter: wgpu::FilterMode, label: &str| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        let nearest = sampler(wgpu::FilterMode::Nearest, "image-nearest");
        let linear = sampler(wgpu::FilterMode::Linear, "image-linear");
        let ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image-ubo"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image-ubo-bind"),
            layout: &ubo_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        let max_dim = device.limits().max_texture_dimension_2d.max(1);
        let cap = 16 * INSTANCE_BYTES as u64;
        Self {
            vb: vertex_buffer(&device, cap),
            white: Tex::placeholder(&device, &queue, &tex_bgl, &nearest, &linear),
            device,
            queue,
            pipeline,
            tex_bgl,
            nearest,
            linear,
            ubo,
            bind,
            cap,
            bytes: Vec::new(),
            cache: HashMap::new(),
            frame: 0,
            panes: Vec::new(),
            max_dim,
        }
    }

    /// Textures cached right now.
    pub(crate) fn cached(&self) -> usize {
        self.cache.len()
    }

    fn upload(&mut self, q: &ImageQuad<'_>) {
        if let Some(t) = self.cache.get_mut(&q.key) {
            t.used = self.frame;
            return;
        }
        let (w, h, px) = prepare_pixels(q.width, q.height, q.rgba, self.max_dim);
        let mut t = Tex::new(
            &self.device,
            &self.queue,
            &self.tex_bgl,
            (&self.nearest, &self.linear),
            (w, h),
            &px,
        );
        t.used = self.frame;
        self.cache.insert(q.key, t);
    }

    fn push(&mut self, dst: [f32; 4], uv: [f32; 4], tint: [f32; 4]) -> u32 {
        let i = (self.bytes.len() / INSTANCE_BYTES) as u32;
        for v in dst.iter().chain(&uv).chain(&tint) {
            self.bytes.extend_from_slice(&v.to_le_bytes());
        }
        i
    }

    fn quad(&mut self, q: &ImageQuad<'_>, origin: (i32, i32)) -> (u32, u64, bool) {
        self.upload(q);
        let (iw, ih) = (q.width.max(1) as f32, q.height.max(1) as f32);
        let [sx, sy, sw, sh] = q.src.map(|v| v as f32);
        let dst = [
            (q.dst[0] + origin.0) as f32,
            (q.dst[1] + origin.1) as f32,
            q.dst[2] as f32,
            q.dst[3] as f32,
        ];
        let uv = [sx / iw, sy / ih, (sx + sw) / iw, (sy + sh) / ih];
        let nearest = q.dst[2] as u32 == q.src[2] && q.dst[3] as u32 == q.src[3];
        (self.push(dst, uv, [1.0; 4]), q.key, nearest)
    }

    /// Pack every pane's quads, upload new textures, retire idle ones. `panes` gives, per
    /// pane: origin, the grid rect size (for the backdrop), backdrop colour, below, above.
    #[allow(clippy::type_complexity)]
    pub(crate) fn prepare(
        &mut self,
        target_size: (u32, u32),
        panes: &[(
            (i32, i32),
            (u32, u32),
            Option<[f32; 4]>,
            &[ImageQuad<'_>],
            &[ImageQuad<'_>],
        )],
    ) {
        self.frame += 1;
        self.bytes.clear();
        self.panes.clear();
        for (origin, size, backdrop, below, above) in panes {
            let mut r = PaneRanges::default();
            if let Some(c) = backdrop {
                let dst = [
                    origin.0 as f32,
                    origin.1 as f32,
                    size.0 as f32,
                    size.1 as f32,
                ];
                r.below
                    .push((self.push(dst, [0.0, 0.0, 1.0, 1.0], *c), 0, true));
            }
            for q in below.iter() {
                let e = self.quad(q, *origin);
                r.below.push(e);
            }
            for q in above.iter() {
                let e = self.quad(q, *origin);
                r.above.push(e);
            }
            self.panes.push(r);
        }
        let need = self.bytes.len() as u64;
        if need > self.cap {
            self.cap = need.next_power_of_two();
            self.vb = vertex_buffer(&self.device, self.cap);
        }
        if need > 0 {
            self.queue.write_buffer(&self.vb, 0, &self.bytes);
            let mut u = [0u8; 16];
            u[0..4].copy_from_slice(&(target_size.0 as f32).to_le_bytes());
            u[4..8].copy_from_slice(&(target_size.1 as f32).to_le_bytes());
            self.queue.write_buffer(&self.ubo, 0, &u);
        }
        self.retire();
    }

    fn retire(&mut self) {
        let frame = self.frame;
        self.cache
            .retain(|_, t| frame.saturating_sub(t.used) <= IDLE_FRAMES);
        let mut total: u64 = self.cache.values().map(|t| t.bytes).sum();
        if total <= CACHE_BYTES {
            return;
        }
        let mut idle: Vec<(u64, u64)> = self
            .cache
            .iter()
            .filter(|(_, t)| t.used != frame)
            .map(|(k, t)| (t.used, *k))
            .collect();
        idle.sort_unstable();
        for (_, k) in idle {
            if total <= CACHE_BYTES {
                break;
            }
            if let Some(t) = self.cache.remove(&k) {
                total -= t.bytes;
            }
        }
    }

    /// True when pane `i` draws anything in `band`.
    pub(crate) fn has(&self, i: usize, band: Band) -> bool {
        self.panes.get(i).is_some_and(|r| match band {
            Band::Below => !r.below.is_empty(),
            Band::Above => !r.above.is_empty(),
        })
    }

    /// Record pane `i`'s quads of `band` into `pass` (scissor already set).
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, i: usize, band: Band) {
        let Some(r) = self.panes.get(i) else {
            return;
        };
        let list = match band {
            Band::Below => &r.below,
            Band::Above => &r.above,
        };
        if list.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.vb.slice(..));
        for &(inst, key, nearest) in list {
            let t = if key == 0 {
                &self.white
            } else if let Some(t) = self.cache.get(&key) {
                t
            } else {
                continue;
            };
            pass.set_bind_group(1, if nearest { &t.nearest } else { &t.linear }, &[]);
            pass.draw(0..4, inst..inst + 1);
        }
    }
}

impl Tex {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bgl: &wgpu::BindGroupLayout,
        samplers: (&wgpu::Sampler, &wgpu::Sampler),
        size: (u32, u32),
        px: &[u8],
    ) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            px,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size.0 * 4),
                rows_per_image: Some(size.1),
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = |s: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("image-bind"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(s),
                    },
                ],
            })
        };
        Tex {
            nearest: bind(samplers.0),
            linear: bind(samplers.1),
            _tex: tex,
            bytes: u64::from(size.0) * u64::from(size.1) * 4,
            used: 0,
        }
    }

    fn placeholder(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bgl: &wgpu::BindGroupLayout,
        nearest: &wgpu::Sampler,
        linear: &wgpu::Sampler,
    ) -> Self {
        Tex::new(
            device,
            queue,
            bgl,
            (nearest, linear),
            (1, 1),
            &[255, 255, 255, 255],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_are_premultiplied() {
        let (w, h, p) = prepare_pixels(1, 1, &[200, 100, 50, 128], 16);
        assert_eq!((w, h), (1, 1));
        assert_eq!(p, vec![100, 50, 25, 128]);
    }

    #[test]
    fn oversize_images_downscale_to_the_device_limit() {
        let px = vec![255u8; 40 * 10 * 4];
        let (w, h, p) = prepare_pixels(40, 10, &px, 8);
        assert_eq!((w, h), (8, 2));
        assert_eq!(p.len(), 8 * 2 * 4);
    }
}
