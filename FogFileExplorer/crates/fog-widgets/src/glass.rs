// SPDX-License-Identifier: AGPL-3.0-only

//! Floating glass (FOG §Visual design, "Floating glass"): a rounded panel
//! with an edge highlight, a soft shadow and grain, drawn by a custom wgpu
//! primitive, with its content in a layer above it.
//!
//! This is a widget and not a styled container because a container is a
//! quad: it cannot sample what is beneath it, light its rim by direction,
//! add grain, or fade as one piece with its shadow.
//!
//! **The backdrop.** iced's surface is a render attachment only: a primitive
//! cannot read the frame it is drawn into. A panel that floats over Fog's
//! own content (the palette, a dialog) is given a [`Backdrop`] instead: a
//! screenshot of the window taken as the panel opens, blurred once with the
//! compositor's dual-Kawase chain (see `kawase.wgsl`) and cached on the GPU
//! until the next one. Panels beside the content (sidebar, path bar, tray)
//! have nothing of Fog's beneath them; what is behind the window is the
//! desktop, which the compositor blurs.
//!
//! A filled panel is drawn in two passes: the interior replaces what is
//! under it, weighted by opacity through the blend constant, so the sharp
//! content fades out as the blurred copy fades in; then the edge, rim and
//! shadow blend over.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse;
use iced::widget::shader::{self, Viewport};
use iced::window::Screenshot;
use iced::{wgpu, Color, Element, Event, Length, Rectangle, Size, Transformation, Vector};

/// How a panel looks. Logical pixels; colours straight-alpha sRGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Body tint. Its alpha is the glass's thickness.
    pub tint: Color,
    pub radius: f32,
    /// Rim highlight colour; alpha is its strength where the light hits.
    pub rim: Color,
    pub rim_width: f32,
    /// White pooled at the top edge, 0 to 1.
    pub sheen: f32,
    /// Grain amplitude, 0 to 1.
    pub grain: f32,
    pub shadow: Color,
    pub shadow_offset: Vector,
    pub shadow_blur: f32,
}

/// Dual-Kawase parameters, the compositor's `decoration.blur`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blur {
    pub size: u32,
    pub passes: u32,
}

impl Blur {
    /// abyss's `sample_offset`: a quarter of `size`, at least one pixel.
    pub fn offset(&self) -> f32 {
        (self.size.clamp(1, 64) as f32 / 4.0).max(1.0)
    }

    pub fn passes(&self) -> u32 {
        self.passes.clamp(1, 6)
    }
}

/// A window snapshot to blur under floating panels.
#[derive(Debug)]
pub struct Backdrop {
    id: u64,
    shot: Screenshot,
    blur: Blur,
}

impl Backdrop {
    pub fn new(shot: Screenshot, blur: Blur) -> Arc<Backdrop> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Backdrop {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            shot,
            blur,
        })
    }

    /// Physical size.
    pub fn size(&self) -> Size<u32> {
        self.shot.size
    }
}

/// A glass panel around `content`.
pub fn glass<'a, Message, Theme>(
    content: impl Into<Element<'a, Message, Theme, iced::Renderer>>,
    style: Style,
) -> Glass<'a, Message, Theme> {
    Glass {
        content: content.into(),
        style,
        backdrop: None,
        opacity: 1.0,
        scale: 1.0,
    }
}

pub struct Glass<'a, Message, Theme> {
    content: Element<'a, Message, Theme, iced::Renderer>,
    style: Style,
    backdrop: Option<Arc<Backdrop>>,
    opacity: f32,
    scale: f32,
}

impl<Message, Theme> Glass<'_, Message, Theme> {
    /// Blur this snapshot of the window under the panel.
    pub fn backdrop(mut self, b: Option<Arc<Backdrop>>) -> Self {
        self.backdrop = b;
        self
    }

    /// Panel opacity, for fades. The content fades by its own colours.
    pub fn opacity(mut self, o: f32) -> Self {
        self.opacity = o.clamp(0.0, 1.0);
        self
    }

    /// Scale about the centre, for springs. Hit testing is unscaled.
    pub fn scale(mut self, s: f32) -> Self {
        self.scale = s;
        self
    }

    fn margin(&self) -> f32 {
        let s = &self.style;
        s.shadow_blur + s.shadow_offset.x.abs().max(s.shadow_offset.y.abs()) + 2.0
    }
}

impl<Message, Theme> Widget<Message, Theme, iced::Renderer> for Glass<'_, Message, Theme> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if self.opacity <= 1e-3 {
            return;
        }
        let bounds = layout.bounds();
        let margin = self.margin();
        let primitive = Primitive {
            style: self.style,
            width: bounds.width,
            margin,
            opacity: self.opacity,
            backdrop: self.backdrop.clone(),
            slot: AtomicU32::new(u32::MAX),
        };
        let paint = |r: &mut iced::Renderer| {
            use iced_wgpu::primitive::Renderer as _;
            r.draw_primitive(bounds.expand(margin), primitive);
            r.with_layer(bounds.expand(1.0), |r| {
                self.content
                    .as_widget()
                    .draw(tree, r, theme, style, layout, cursor, viewport);
            });
        };
        if (self.scale - 1.0).abs() > 1e-3 {
            let c = bounds.center();
            let t = Transformation::translate(c.x, c.y)
                * Transformation::scale(self.scale)
                * Transformation::translate(-c.x, -c.y);
            renderer.with_transformation(t, paint);
        } else {
            paint(renderer);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a, Theme: 'a> From<Glass<'a, Message, Theme>>
    for Element<'a, Message, Theme, iced::Renderer>
{
    fn from(g: Glass<'a, Message, Theme>) -> Self {
        Element::new(g)
    }
}

/// One panel, drawn by [`Pipeline`].
#[derive(Debug)]
struct Primitive {
    style: Style,
    /// Unscaled body width: the ratio to the drawn width is the scale.
    width: f32,
    margin: f32,
    opacity: f32,
    backdrop: Option<Arc<Backdrop>>,
    /// First uniform slot this frame; a filled panel uses two.
    slot: AtomicU32,
}

/// Seven `vec4<f32>`, padded to the dynamic-offset alignment.
const UNIFORM: u64 = 7 * 16;
const STRIDE: u64 = 256;

type Uniform = [[f32; 4]; 7];

fn linear(c: Color) -> [f32; 4] {
    let l = c.into_linear();
    [l[0] * l[3], l[1] * l[3], l[2] * l[3], l[3]]
}

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let sf = viewport.scale_factor();
        // `bounds` is transformed: recover the scale from the width.
        let body = bounds.shrink(self.margin * (bounds.width / (self.width + 2.0 * self.margin)));
        let k = body.width / self.width.max(1.0) * sf;
        let s = &self.style;
        let mut backdrop_size = [1.0, 1.0];
        if let Some(b) = &self.backdrop {
            pipeline.blur(device, queue, b);
            backdrop_size = [b.shot.size.width as f32, b.shot.size.height as f32];
        }
        let mode = if self.backdrop.is_some() { 1.0 } else { 0.0 };
        let mut u: Uniform = [
            [body.x * sf, body.y * sf, body.width * sf, body.height * sf],
            linear(s.tint),
            {
                let l = s.rim.into_linear();
                [l[0], l[1], l[2], s.rim.a]
            },
            linear(s.shadow),
            [
                s.shadow_offset.x * k,
                s.shadow_offset.y * k,
                s.shadow_blur * k,
                s.rim_width * k,
            ],
            [s.radius * k, s.grain, self.opacity, mode],
            [
                backdrop_size[0],
                backdrop_size[1],
                if pipeline.encode { 1.0 } else { 0.0 },
                s.sheen,
            ],
        ];
        let slot = pipeline.push(device, queue, &u);
        if self.backdrop.is_some() {
            u[5][3] = 2.0;
            pipeline.push(device, queue, &u);
        }
        self.slot.store(slot, Ordering::Relaxed);
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let slot = self.slot.load(Ordering::Relaxed);
        if slot == u32::MAX {
            return true;
        }
        let texture = match (&self.backdrop, &pipeline.backdrop) {
            (Some(b), Some(c)) if c.id == b.id => &c.group,
            (Some(_), _) => return true,
            (None, _) => &pipeline.empty,
        };
        pass.set_bind_group(1, texture, &[]);
        if self.backdrop.is_some() {
            let o = f64::from(self.opacity);
            pass.set_pipeline(&pipeline.fill);
            pass.set_blend_constant(wgpu::Color {
                r: o,
                g: o,
                b: o,
                a: o,
            });
            pass.set_bind_group(0, &pipeline.group, &[slot * STRIDE as u32]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&pipeline.over);
            pass.set_bind_group(0, &pipeline.group, &[(slot + 1) * STRIDE as u32]);
            pass.draw(0..3, 0..1);
        } else {
            pass.set_pipeline(&pipeline.over);
            pass.set_bind_group(0, &pipeline.group, &[slot * STRIDE as u32]);
            pass.draw(0..3, 0..1);
        }
        true
    }
}

/// The blurred snapshot on the GPU.
struct Cached {
    id: u64,
    group: wgpu::BindGroup,
}

pub struct Pipeline {
    over: wgpu::RenderPipeline,
    fill: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    kawase_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
    /// Slots the buffer holds, and those used this frame (kept so a grown
    /// buffer can be refilled).
    capacity: u32,
    frame: Vec<Uniform>,
    empty: wgpu::BindGroup,
    backdrop: Option<Cached>,
    /// Kawase uniform buffers, one per pass index, reused by every rebuild.
    scratch: Vec<wgpu::Buffer>,
    /// The target is not sRGB: the shader encodes.
    encode: bool,
}

/// Blur intermediates: filterable, renderable everywhere, no banding.
const LEVEL: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let panel = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fog.glass.panel"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glass/panel.wgsl").into()),
        });
        let kawase = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fog.glass.kawase"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glass/kawase.wgsl").into()),
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fog.glass.uniform"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM),
                },
                count: None,
            }],
        });
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let smp = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fog.glass.texture"),
            entries: &[tex(0), smp(1)],
        });
        let kawase_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fog.glass.kawase"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex(1),
                smp(2),
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fog.glass.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let panel_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fog.glass.panel"),
            bind_group_layouts: &[&uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let premultiplied = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let constant = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Constant,
            dst_factor: wgpu::BlendFactor::OneMinusConstant,
            operation: wgpu::BlendOperation::Add,
        };
        let make = |label, layout: &wgpu::PipelineLayout, module, fs, format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        let over = make(
            "fog.glass.over",
            &panel_layout,
            &panel,
            "fs",
            format,
            Some(wgpu::BlendState {
                color: premultiplied,
                alpha: premultiplied,
            }),
        );
        let fill = make(
            "fog.glass.fill",
            &panel_layout,
            &panel,
            "fs",
            format,
            Some(wgpu::BlendState {
                color: constant,
                alpha: constant,
            }),
        );
        let kawase_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fog.glass.kawase"),
            bind_group_layouts: &[&kawase_layout],
            push_constant_ranges: &[],
        });
        let down = make("fog.glass.down", &kawase_pl, &kawase, "down", LEVEL, None);
        let up = make("fog.glass.up", &kawase_pl, &kawase, "up", LEVEL, None);

        let capacity = 16;
        let buffer = uniform_buffer(device, capacity);
        let group = uniform_group(device, &uniform_layout, &buffer);
        let blank = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fog.glass.blank"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LEVEL,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let empty = texture_group(device, &texture_layout, &blank, &sampler);

        Pipeline {
            over,
            fill,
            down,
            up,
            uniform_layout,
            texture_layout,
            kawase_layout,
            sampler,
            buffer,
            group,
            capacity,
            frame: Vec::new(),
            empty,
            backdrop: None,
            scratch: Vec::new(),
            encode: !format.is_srgb(),
        }
    }

    fn trim(&mut self) {
        self.frame.clear();
    }
}

fn uniform_buffer(device: &wgpu::Device, slots: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fog.glass.uniforms"),
        size: STRIDE * u64::from(slots),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fog.glass.uniforms"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: wgpu::BufferSize::new(UNIFORM),
            }),
        }],
    })
}

fn texture_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    texture: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fog.glass.texture"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn bytes(u: &Uniform) -> Vec<u8> {
    u.iter().flatten().flat_map(|f| f.to_ne_bytes()).collect()
}

impl Pipeline {
    /// Queue one panel's uniforms; returns its slot.
    fn push(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, u: &Uniform) -> u32 {
        let slot = self.frame.len() as u32;
        self.frame.push(*u);
        if slot >= self.capacity {
            self.capacity = (slot + 1).next_power_of_two();
            self.buffer = uniform_buffer(device, self.capacity);
            self.group = uniform_group(device, &self.uniform_layout, &self.buffer);
            for (i, u) in self.frame.iter().enumerate() {
                queue.write_buffer(&self.buffer, i as u64 * STRIDE, &bytes(u));
            }
        } else {
            queue.write_buffer(&self.buffer, u64::from(slot) * STRIDE, &bytes(u));
        }
        slot
    }

    /// Upload and blur `b`, unless it is the one already blurred.
    fn blur(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, b: &Backdrop) {
        if self.backdrop.as_ref().is_some_and(|c| c.id == b.id) {
            return;
        }
        let Size { width, height } = b.shot.size;
        if width == 0 || height == 0 || b.shot.rgba.len() < (width * height * 4) as usize {
            return;
        }
        let texture = |label, w: u32, h: u32, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w.max(1),
                    height: h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let binding = wgpu::TextureUsages::TEXTURE_BINDING;
        let target = wgpu::TextureUsages::RENDER_ATTACHMENT | binding;
        // The screenshot is sRGB-encoded premultiplied RGBA.
        let source = texture(
            "fog.glass.snapshot",
            width,
            height,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            binding | wgpu::TextureUsages::COPY_DST,
        );
        queue.write_texture(
            source.as_image_copy(),
            &b.shot.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let passes = b.blur.passes().min(31 - width.min(height).leading_zeros());
        let mut levels = vec![source];
        for i in 1..=passes {
            levels.push(texture(
                "fog.glass.level",
                width >> i,
                height >> i,
                LEVEL,
                target,
            ));
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fog.glass.blur"),
        });
        let offset = b.blur.offset();
        // One uniform buffer per pass index, kept across rebuilds.
        let mut scratch = std::mem::take(&mut self.scratch);
        let mut next = 0;
        let mut run =
            |pipeline: &wgpu::RenderPipeline, src: &wgpu::Texture, dst: &wgpu::Texture| {
                let size = src.size();
                let mut data = Vec::with_capacity(16);
                for f in [
                    0.5 / size.width as f32,
                    0.5 / size.height as f32,
                    offset,
                    0.0,
                ] {
                    data.extend_from_slice(&f.to_ne_bytes());
                }
                if next == scratch.len() {
                    scratch.push(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("fog.glass.kawase"),
                        size: 16,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
                }
                let uniform = &scratch[next];
                next += 1;
                queue.write_buffer(uniform, 0, &data);
                let view = src.create_view(&wgpu::TextureViewDescriptor::default());
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog.glass.kawase"),
                    layout: &self.kawase_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                let out = dst.create_view(&wgpu::TextureViewDescriptor::default());
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fog.glass.kawase"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &out,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
            };
        for i in 1..levels.len() {
            run(&self.down, &levels[i - 1], &levels[i]);
        }
        for i in (2..levels.len()).rev() {
            run(&self.up, &levels[i], &levels[i - 1]);
        }
        // The result is level 1 (half size, bilinear-sampled by uv); a
        // 1px-wide snapshot has no levels and is used as is.
        let result = &levels[levels.len().min(2) - 1];
        self.scratch = scratch;
        queue.submit([encoder.finish()]);

        self.backdrop = Some(Cached {
            id: b.id,
            group: texture_group(device, &self.texture_layout, result, &self.sampler),
        });
    }
}
