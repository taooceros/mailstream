//! Local quarter-resolution 2-pass Dual-Kawase backdrop blur for floating glass
//! capsules (`compact-bottom-bar`, `compact_capsule`, `compact-reply-bar`).
//!
//! Capped at [`MAX_REGIONS_PER_FRAME`] small regions per frame. Each active
//! region copies its padded bounding box from the swapchain texture (requiring
//! `wgpu::TextureUsages::COPY_SRC`, falling back transparently to the quad's
//! own `Background` fill when unsupported), downsamples 4× into a quarter-res
//! target (~51 KiB for a 1080×192 px dock), runs an 8-tap Dual-Kawase upsample,
//! and composites the blurred backdrop + `1/255` triangular Void-and-Cluster
//! blue-noise dither + translucent glass tint with exact rounded-SDF edges.

use gpui::{Bounds, ColorSpace, Quad, ScaledPixels};

use crate::wgpu_renderer::WgpuRendererCore;

/// Maximum number of local backdrop-blur regions evaluated in a single frame.
pub(crate) const MAX_REGIONS_PER_FRAME: usize = 2;

/// Consecutive frames without any backdrop-blur quad after which scratch
/// textures are released (~1.5 s at 120 Hz).
pub(crate) const BACKDROP_BLUR_IDLE_RELEASE_FRAMES: u64 = 180;

/// Scales the capsule's opaque-fallback tint alpha when real frosted backdrop
/// blur is active, so blurred scrolling content glides visibly through while
/// foreground icons and labels stay legible.
const FROSTED_TINT_ALPHA_SCALE: f32 = 0.78;

/// A candidate backdrop-blur quad selected for this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SelectedRegion {
    pub(crate) step_index: usize,
    pub(crate) quad_index: usize,
    pub(crate) slot: usize,
    area: u64,
}

/// Up to [`MAX_REGIONS_PER_FRAME`] selected backdrop-blur quads for one frame.
#[derive(Default)]
pub(crate) struct SelectedRegions {
    entries: [Option<SelectedRegion>; MAX_REGIONS_PER_FRAME],
    len: usize,
}

impl SelectedRegions {
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = SelectedRegion> + '_ {
        self.entries[..self.len].iter().filter_map(|entry| *entry)
    }

    pub(crate) fn for_step(&self, step_index: usize) -> [Option<SelectedRegion>; MAX_REGIONS_PER_FRAME] {
        let mut out = [None; MAX_REGIONS_PER_FRAME];
        let mut count = 0;
        for entry in self.iter() {
            if entry.step_index == step_index {
                out[count] = Some(entry);
                count += 1;
            }
        }
        // Sort by ascending quad_index so the caller splits the batch in order.
        if count == 2
            && let (Some(a), Some(b)) = (out[0], out[1])
            && a.quad_index > b.quad_index
        {
            out.swap(0, 1);
        }
        out
    }

    fn consider(&mut self, step_index: usize, quad_index: usize, area: u64) {
        if self.len < MAX_REGIONS_PER_FRAME {
            let slot = self.len;
            self.entries[self.len] = Some(SelectedRegion {
                step_index,
                quad_index,
                slot,
                area,
            });
            self.len += 1;
            return;
        }
        // Replace the smallest existing entry if this candidate covers more area
        // (so the primary bottom dock always wins over tiny icon capsules).
        let mut min_idx = 0;
        let mut min_area = self.entries[0].map_or(u64::MAX, |e| e.area);
        for (i, entry) in self.entries.iter().enumerate().skip(1) {
            let entry_area = entry.map_or(u64::MAX, |e| e.area);
            if entry_area < min_area {
                min_area = entry_area;
                min_idx = i;
            }
        }
        if area > min_area {
            let slot = self.entries[min_idx].map_or(min_idx, |e| e.slot);
            self.entries[min_idx] = Some(SelectedRegion {
                step_index,
                quad_index,
                slot,
                area,
            });
        }
    }
}

/// Computes the clipped integer pixel rect of a quad within `(frame_w, frame_h)`.
fn clipped_pixel_rect(
    bounds: Bounds<ScaledPixels>,
    mask: Bounds<ScaledPixels>,
    frame_w: u32,
    frame_h: u32,
) -> Option<(u32, u32, u32, u32)> {
    let clipped = bounds.intersect(&mask);
    if clipped.is_empty() {
        return None;
    }
    let x0 = clipped.origin.x.0.floor().clamp(0.0, frame_w as f32) as u32;
    let y0 = clipped.origin.y.0.floor().clamp(0.0, frame_h as f32) as u32;
    let x1 = clipped.right().0.ceil().clamp(0.0, frame_w as f32) as u32;
    let y1 = clipped.bottom().0.ceil().clamp(0.0, frame_h as f32) as u32;
    let w = x1.saturating_sub(x0);
    let h = y1.saturating_sub(y0);
    if w < 4 || h < 4 {
        return None;
    }
    Some((x0, y0, w, h))
}

/// Selects up to [`MAX_REGIONS_PER_FRAME`] small backdrop-blur quads in a frame.
pub(crate) fn select_regions<'a>(
    frame_w: u32,
    frame_h: u32,
    batches: impl Iterator<Item = (usize, &'a [Quad], std::ops::Range<usize>)>,
) -> SelectedRegions {
    let mut selected = SelectedRegions::default();
    let frame_area = u64::from(frame_w) * u64::from(frame_h);
    if frame_area == 0 {
        return selected;
    }
    // Cap individual local blur regions at 50% of the viewport so only small
    // floating UI elements (docks, capsules) take the fast local copy path.
    let max_region_area = (frame_area / 2).max(1);

    for (step_index, quads, range) in batches {
        for quad_index in range {
            let quad = &quads[quad_index];
            if !quad.background.has_backdrop_blur() {
                continue;
            }
            let Some((_, _, w, h)) =
                clipped_pixel_rect(quad.bounds, quad.content_mask.bounds, frame_w, frame_h)
            else {
                continue;
            };
            let area = u64::from(w) * u64::from(h);
            if area <= max_region_area {
                selected.consider(step_index, quad_index, area);
            }
        }
    }
    selected
}

/// Uniform parameters for Dual-Kawase downsample / upsample passes (32 bytes,
/// 16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KawaseParams {
    /// `(src_uv_max_x, src_uv_max_y, halfpixel_x * offset, halfpixel_y * offset)`
    uv_and_step: [f32; 4],
    /// `(min_u, min_v, max_u, max_v)` strictly within the valid copied sub-rect.
    uv_clamp: [f32; 4],
}

/// Uniform parameters for compositing the blurred backdrop + frosted glass tint
/// with rounded-SDF edges and `1/255` blue-noise dithering (160 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeParams {
    /// `(viewport_w, viewport_h, premultiplied_alpha, is_linear_gradient)`
    viewport_and_flags: [f32; 4],
    /// `(origin_x, origin_y, width, height)`
    bounds: [f32; 4],
    /// `(origin_x, origin_y, width, height)`
    content_mask: [f32; 4],
    /// `(top_left, top_right, bottom_right, bottom_left)`
    corner_radii: [f32; 4],
    /// `(src_x, src_y, src_w, src_h)` of the copied backdrop sub-rect in pixels.
    src_rect: [f32; 4],
    /// `(min_u, min_v, max_u, max_v)` clamped within `quarter_b`.
    uv_clamp: [f32; 4],
    /// `(q_uv_max_u, q_uv_max_v, gradient_angle_deg, color_space)`
    uv_scale_and_grad: [f32; 4],
    /// Pre-converted sRGB RGBA of tint stop 0 (or solid tint).
    tint_color0: [f32; 4],
    /// Pre-converted sRGB RGBA of tint stop 1.
    tint_color1: [f32; 4],
    /// `(stop0_pct, stop1_pct, frosted_alpha_scale, 0.0)`
    tint_stops: [f32; 4],
}

/// GPU pipelines and bind group layouts shared by all local backdrop blurs.
pub(crate) struct BackdropBlurPipelines {
    kawase_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    downsample: wgpu::RenderPipeline,
    upsample: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
}

impl BackdropBlurPipelines {
    pub(crate) fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        alpha_mode: wgpu::CompositeAlphaMode,
        pipeline_cache: Option<&wgpu::PipelineCache>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("backdrop_blur_shader"),
            source: wgpu::ShaderSource::Wgsl(BACKDROP_BLUR_SHADER.into()),
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("backdrop_blur_linear_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let kawase_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop_kawase_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(size_of::<KawaseParams>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("backdrop_composite_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            size_of::<CompositeParams>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let kawase_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("backdrop_kawase_pipeline_layout"),
                bind_group_layouts: &[Some(&kawase_layout)],
                immediate_size: 0,
            });

        let make_kawase = |label: &str, fs_entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&kawase_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_fullscreen"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs_entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: pipeline_cache,
            })
        };

        let downsample = make_kawase("backdrop_kawase_downsample", "fs_kawase_down");
        let upsample = make_kawase("backdrop_kawase_upsample", "fs_kawase_up");

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("backdrop_composite_pipeline_layout"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            });

        let blend_mode = match alpha_mode {
            wgpu::CompositeAlphaMode::PreMultiplied => {
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
            }
            _ => wgpu::BlendState::ALPHA_BLENDING,
        };

        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("backdrop_composite_pipeline"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_composite"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_composite"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(blend_mode),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: pipeline_cache,
        });

        Self {
            kawase_layout,
            composite_layout,
            sampler,
            downsample,
            upsample,
            composite,
        }
    }
}

/// Scratch textures and uniform buffers for one local backdrop-blur slot.
struct BlurSlot {
    format: wgpu::TextureFormat,
    src_tex: wgpu::Texture,
    src_w: u32,
    src_h: u32,
    _quarter_a: wgpu::Texture,
    quarter_a_view: wgpu::TextureView,
    _quarter_b: wgpu::Texture,
    quarter_b_view: wgpu::TextureView,
    q_w: u32,
    q_h: u32,
    down_uniform: wgpu::Buffer,
    up_uniform: wgpu::Buffer,
    comp_uniform: wgpu::Buffer,
    down_bg: wgpu::BindGroup,
    up_bg: wgpu::BindGroup,
    comp_bg: wgpu::BindGroup,
}

impl BlurSlot {
    fn matches(&self, format: wgpu::TextureFormat, req_src_w: u32, req_src_h: u32, req_q_w: u32, req_q_h: u32) -> bool {
        self.format == format
            && self.src_w >= req_src_w
            && self.src_h >= req_src_h
            && self.src_w <= req_src_w.saturating_mul(2).max(128)
            && self.src_h <= req_src_h.saturating_mul(2).max(128)
            && self.q_w >= req_q_w
            && self.q_h >= req_q_h
    }

    fn new(
        device: &wgpu::Device,
        pipelines: &BackdropBlurPipelines,
        format: wgpu::TextureFormat,
        req_src_w: u32,
        req_src_h: u32,
        req_q_w: u32,
        req_q_h: u32,
    ) -> Self {
        let src_w = req_src_w.next_multiple_of(32).max(32);
        let src_h = req_src_h.next_multiple_of(32).max(32);
        let q_w = req_q_w.next_multiple_of(16).max(16);
        let q_h = req_q_h.next_multiple_of(16).max(16);

        let src_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop_blur_src"),
            size: wgpu::Extent3d {
                width: src_w,
                height: src_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let src_view = src_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let make_quarter = |label: &str| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: q_w,
                    height: q_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            (tex, view)
        };

        let (quarter_a, quarter_a_view) = make_quarter("backdrop_blur_quarter_a");
        let (quarter_b, quarter_b_view) = make_quarter("backdrop_blur_quarter_b");

        let make_uniform = |label: &str, size: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        let down_uniform = make_uniform("backdrop_down_uniform", size_of::<KawaseParams>());
        let up_uniform = make_uniform("backdrop_up_uniform", size_of::<KawaseParams>());
        let comp_uniform = make_uniform("backdrop_comp_uniform", size_of::<CompositeParams>());

        let down_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop_down_bg"),
            layout: &pipelines.kawase_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: down_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
            ],
        });

        let up_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop_up_bg"),
            layout: &pipelines.kawase_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: up_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&quarter_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
            ],
        });

        let comp_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop_comp_bg"),
            layout: &pipelines.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: comp_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&quarter_b_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
            ],
        });

        Self {
            format,
            src_tex,
            src_w,
            src_h,
            _quarter_a: quarter_a,
            quarter_a_view,
            _quarter_b: quarter_b,
            quarter_b_view,
            q_w,
            q_h,
            down_uniform,
            up_uniform,
            comp_uniform,
            down_bg,
            up_bg,
            comp_bg,
        }
    }
}

#[derive(Default)]
pub(crate) struct BackdropBlurState {
    slots: [Option<BlurSlot>; MAX_REGIONS_PER_FRAME],
    frame: u64,
    last_used_frame: Option<u64>,
}

impl BackdropBlurState {
    /// Releases all cached backdrop-blur scratch textures immediately
    /// (called on `unconfigure_surface` and `trim_memory`).
    pub(crate) fn clear(&mut self) {
        self.slots = [None, None];
        self.last_used_frame = None;
    }

    #[cfg(test)]
    pub(crate) fn allocated_slot_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Advances the frame counter and drops idle scratch textures after
    /// [`BACKDROP_BLUR_IDLE_RELEASE_FRAMES`] consecutive unused frames.
    pub(crate) fn note_frame(&mut self, used: bool) {
        self.frame += 1;
        if used {
            self.last_used_frame = Some(self.frame);
        } else if let Some(last_used) = self.last_used_frame
            && self.frame.saturating_sub(last_used) >= BACKDROP_BLUR_IDLE_RELEASE_FRAMES
        {
            self.clear();
        }
    }

    /// Copies the quad's padded backdrop region from `frame_texture`, executes
    /// the 2-pass quarter-resolution Dual-Kawase blur (downsample + upsample),
    /// and updates the composite uniform buffer for `slot_index`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_and_blur(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &BackdropBlurPipelines,
        encoder: &mut wgpu::CommandEncoder,
        frame_texture: &wgpu::Texture,
        format: wgpu::TextureFormat,
        premultiplied_alpha: bool,
        quad: &Quad,
        slot_index: usize,
    ) -> bool {
        if slot_index >= MAX_REGIONS_PER_FRAME {
            return false;
        }
        let frame_w = frame_texture.width();
        let frame_h = frame_texture.height();
        let Some((x0, y0, w, h)) =
            clipped_pixel_rect(quad.bounds, quad.content_mask.bounds, frame_w, frame_h)
        else {
            return false;
        };

        let blur_radius = quad.background.backdrop_blur_radius();
        let pad = (blur_radius.ceil() as u32).clamp(8, 24);
        let src_x = x0.saturating_sub(pad);
        let src_y = y0.saturating_sub(pad);
        let src_right = (x0 + w + pad).min(frame_w);
        let src_bottom = (y0 + h + pad).min(frame_h);
        let src_w = src_right.saturating_sub(src_x);
        let src_h = src_bottom.saturating_sub(src_y);
        if src_w < 4 || src_h < 4 {
            return false;
        }

        let q_w = (src_w / 4).max(1);
        let q_h = (src_h / 4).max(1);

        let needs_alloc = self.slots[slot_index]
            .as_ref()
            .is_none_or(|slot| !slot.matches(format, src_w, src_h, q_w, q_h));
        if needs_alloc {
            self.slots[slot_index] = Some(BlurSlot::new(
                device, pipelines, format, src_w, src_h, q_w, q_h,
            ));
        }
        let Some(slot) = self.slots[slot_index].as_ref() else {
            return false;
        };

        // Copy only the padded local region from the current frame texture.
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: frame_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: src_x,
                    y: src_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &slot.src_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: src_w,
                height: src_h,
                depth_or_array_layers: 1,
            },
        );

        let src_uv_max_x = src_w as f32 / slot.src_w as f32;
        let src_uv_max_y = src_h as f32 / slot.src_h as f32;
        let src_half_x = 0.5 / slot.src_w as f32;
        let src_half_y = 0.5 / slot.src_h as f32;
        // Scale Dual-Kawase kernel offset gently with blur radius (in device px).
        let down_offset = (blur_radius / 10.0).clamp(1.2, 3.0);
        let down_params = KawaseParams {
            uv_and_step: [
                src_uv_max_x,
                src_uv_max_y,
                src_half_x * 2.0 * down_offset,
                src_half_y * 2.0 * down_offset,
            ],
            uv_clamp: [
                src_half_x,
                src_half_y,
                (src_uv_max_x - src_half_x).max(src_half_x),
                (src_uv_max_y - src_half_y).max(src_half_y),
            ],
        };
        queue.write_buffer(&slot.down_uniform, 0, bytemuck::bytes_of(&down_params));

        let q_uv_max_x = q_w as f32 / slot.q_w as f32;
        let q_uv_max_y = q_h as f32 / slot.q_h as f32;
        let q_half_x = 0.5 / slot.q_w as f32;
        let q_half_y = 0.5 / slot.q_h as f32;
        let up_offset = (blur_radius / 14.0).clamp(1.0, 2.2);
        let up_params = KawaseParams {
            uv_and_step: [
                q_uv_max_x,
                q_uv_max_y,
                q_half_x * 2.0 * up_offset,
                q_half_y * 2.0 * up_offset,
            ],
            uv_clamp: [
                q_half_x,
                q_half_y,
                (q_uv_max_x - q_half_x).max(q_half_x),
                (q_uv_max_y - q_half_y).max(q_half_y),
            ],
        };
        queue.write_buffer(&slot.up_uniform, 0, bytemuck::bytes_of(&up_params));

        let (is_grad, color_space, angle_deg, c0_hsla, stop0, c1_hsla, stop1) =
            quad.background.backdrop_tint_params();
        let c0 = c0_hsla.to_rgb();
        let c1 = c1_hsla.to_rgb();
        let cs_f32 = match color_space {
            ColorSpace::Srgb => 0.0,
            ColorSpace::Oklab => 1.0,
        };
        let comp_params = CompositeParams {
            viewport_and_flags: [
                frame_w as f32,
                frame_h as f32,
                if premultiplied_alpha { 1.0 } else { 0.0 },
                if is_grad { 1.0 } else { 0.0 },
            ],
            bounds: [
                quad.bounds.origin.x.0,
                quad.bounds.origin.y.0,
                quad.bounds.size.width.0,
                quad.bounds.size.height.0,
            ],
            content_mask: [
                quad.content_mask.bounds.origin.x.0,
                quad.content_mask.bounds.origin.y.0,
                quad.content_mask.bounds.size.width.0,
                quad.content_mask.bounds.size.height.0,
            ],
            corner_radii: [
                quad.corner_radii.top_left.0,
                quad.corner_radii.top_right.0,
                quad.corner_radii.bottom_right.0,
                quad.corner_radii.bottom_left.0,
            ],
            src_rect: [src_x as f32, src_y as f32, src_w as f32, src_h as f32],
            uv_clamp: [
                q_half_x,
                q_half_y,
                (q_uv_max_x - q_half_x).max(q_half_x),
                (q_uv_max_y - q_half_y).max(q_half_y),
            ],
            uv_scale_and_grad: [q_uv_max_x, q_uv_max_y, angle_deg, cs_f32],
            tint_color0: [c0.r, c0.g, c0.b, c0.a],
            tint_color1: [c1.r, c1.g, c1.b, c1.a],
            tint_stops: [stop0, stop1, FROSTED_TINT_ALPHA_SCALE, 0.0],
        };
        queue.write_buffer(&slot.comp_uniform, 0, bytemuck::bytes_of(&comp_params));

        // Pass 1: 5-tap Dual-Kawase downsample into quarter-resolution target A.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop_kawase_downsample_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &slot.quarter_a_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_viewport(0.0, 0.0, q_w as f32, q_h as f32, 0.0, 1.0);
            pass.set_pipeline(&pipelines.downsample);
            pass.set_bind_group(0, &slot.down_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Pass 2: 8-tap Dual-Kawase upsample into quarter-resolution target B.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop_kawase_upsample_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &slot.quarter_b_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_viewport(0.0, 0.0, q_w as f32, q_h as f32, 0.0, 1.0);
            pass.set_pipeline(&pipelines.upsample);
            pass.set_bind_group(0, &slot.up_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        true
    }

    /// Composites the blurred backdrop + translucent glass tint for `slot_index`
    /// into the currently open main render pass.
    pub(crate) fn draw_composite(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipelines: &BackdropBlurPipelines,
        slot_index: usize,
    ) {
        let Some(slot) = self.slots.get(slot_index).and_then(|s| s.as_ref()) else {
            return;
        };
        pass.set_pipeline(&pipelines.composite);
        pass.set_bind_group(0, &slot.comp_bg, &[]);
        pass.draw(0..4, 0..1);
    }
}

/// Checks whether a quad has zero border widths (normal background quad from
/// `Style::paint`), so the composite pass replaces it cleanly without drawing
/// the opaque-fallback gradient on top.
pub(crate) fn quad_has_no_border(quad: &Quad) -> bool {
    quad.border_widths.top.0 == 0.0
        && quad.border_widths.right.0 == 0.0
        && quad.border_widths.bottom.0 == 0.0
        && quad.border_widths.left.0 == 0.0
}

/// Helper used by tests to confirm `WgpuRendererCore` exposes idle release frame constant.
#[allow(dead_code)]
pub(crate) fn _keep_renderer_import(_: &WgpuRendererCore) {}

const BACKDROP_BLUR_SHADER: &str = r#"
struct KawaseParams {
    uv_and_step: vec4<f32>,
    uv_clamp: vec4<f32>,
}

struct CompositeParams {
    viewport_and_flags: vec4<f32>,
    bounds: vec4<f32>,
    content_mask: vec4<f32>,
    corner_radii: vec4<f32>,
    src_rect: vec4<f32>,
    uv_clamp: vec4<f32>,
    uv_scale_and_grad: vec4<f32>,
    tint_color0: vec4<f32>,
    tint_color1: vec4<f32>,
    tint_stops: vec4<f32>,
}

struct FullscreenOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var<uniform> kawase: KawaseParams;
@group(0) @binding(1) var t_src: texture_2d<f32>;
@group(0) @binding(2) var s_src: sampler;

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> FullscreenOut {
    var pos = vec2<f32>(-1.0, -1.0);
    if (vid == 1u) {
        pos = vec2<f32>(3.0, -1.0);
    } else if (vid == 2u) {
        pos = vec2<f32>(-1.0, 3.0);
    }
    var out: FullscreenOut;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = vec2<f32>((pos.x + 1.0) * 0.5, (1.0 - pos.y) * 0.5);
    return out;
}

fn clamp_kawase_uv(uv: vec2<f32>) -> vec2<f32> {
    return clamp(uv, kawase.uv_clamp.xy, kawase.uv_clamp.zw);
}

// 5-tap Dual-Kawase downsample (center * 4 + 4 diagonal corners, / 8).
@fragment
fn fs_kawase_down(in: FullscreenOut) -> @location(0) vec4<f32> {
    let uv = in.uv * kawase.uv_and_step.xy;
    let step = kawase.uv_and_step.zw;
    var sum = textureSample(t_src, s_src, clamp_kawase_uv(uv)) * 4.0;
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv - step));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + step));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(step.x, -step.y)));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(-step.x, step.y)));
    return sum * 0.125;
}

// 8-tap Dual-Kawase upsample (4 cardinal * 1 + 4 diagonal * 2, / 12).
@fragment
fn fs_kawase_up(in: FullscreenOut) -> @location(0) vec4<f32> {
    let uv = in.uv * kawase.uv_and_step.xy;
    let hp = kawase.uv_and_step.zw;
    var sum = textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(-hp.x * 2.0, 0.0)));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(-hp.x, hp.y))) * 2.0;
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(0.0, hp.y * 2.0)));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(hp.x, hp.y))) * 2.0;
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(hp.x * 2.0, 0.0)));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(hp.x, -hp.y))) * 2.0;
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(0.0, -hp.y * 2.0)));
    sum += textureSample(t_src, s_src, clamp_kawase_uv(uv + vec2<f32>(-hp.x, -hp.y))) * 2.0;
    return sum / 12.0;
}

@group(0) @binding(0) var<uniform> comp: CompositeParams;

struct CompositeOut {
    @builtin(position) position: vec4<f32>,
    @location(0) pixel_pos: vec2<f32>,
}

@vertex
fn vs_composite(@builtin(vertex_index) vid: u32) -> CompositeOut {
    let unit = vec2<f32>(f32(vid & 1u), 0.5 * f32(vid & 2u));
    let pixel = comp.bounds.xy + unit * comp.bounds.zw;
    let ndc = pixel / comp.viewport_and_flags.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: CompositeOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.pixel_pos = pixel;
    return out;
}

// Precomputed 16x16 periodic Void-and-Cluster blue-noise tile (4 ranks per u32).
var<private> BLUE_NOISE_16X16: array<u32, 64> = array<u32, 64>(
    0x39eb7f1bu, 0xee2c526bu, 0xd119903eu, 0xacda8c28u, 0xa7259c65u, 0x72b193fbu, 0x7a35ddb9u, 0xc814a054u,
    0x5fd54df8u, 0x4b1dd70au, 0xfaa95e08u, 0x2fe83fbeu, 0x81bd1676u, 0xf27bc145u, 0x692388cau, 0x955c8200u,
    0xf3923db6u, 0x29a26730u, 0xcd47e997u, 0xd021b3e0u, 0x106de405u, 0x5a03e2afu, 0x9a0f6f38u, 0x48f56e34u,
    0xdc2aa863u, 0xffc58b4fu, 0x85bcdbaau, 0x8a9e1a55u, 0x96bf53ecu, 0x7e421e71u, 0xf9274c13u, 0x31c7deadu,
    0x3b1580d3u, 0x64b4edceu, 0x62799dd2u, 0x0e754006u, 0x60fc2698u, 0x91330ca1u, 0xc23cf022u, 0xb7578feau,
    0xd6b06c4au, 0xe3c4517cu, 0x8311ae58u, 0xf41ca52eu, 0x4307e536u, 0x0189f124u, 0xcf50d973u, 0x7dd8ba68u,
    0xc386a4cbu, 0x44b56699u, 0xf68e2da3u, 0x61024620u, 0xf75b2b17u, 0x1fd43718u, 0x7709c9fdu, 0xb287e19bu,
    0x4974df94u, 0x5d78e7abu, 0x3ab84e84u, 0xfe32c059u, 0xcc04bb41u, 0x9fc60d8du, 0xa66ae612u, 0x56700befu,
);

fn sample_blue_noise(coord: vec2<u32>) -> f32 {
    let idx = ((coord.y & 15u) << 4u) | (coord.x & 15u);
    let word = BLUE_NOISE_16X16[idx >> 2u];
    let shift = (idx & 3u) * 8u;
    return f32((word >> shift) & 0xffu) / 255.0;
}

fn srgb_to_linear_3(srgb: vec3<f32>) -> vec3<f32> {
    let c = max(srgb, vec3<f32>(0.0));
    let cutoff = c < vec3<f32>(0.04045);
    let higher = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    let lower = c / vec3<f32>(12.92);
    return select(higher, lower, cutoff);
}

fn linear_to_srgb_3(linear: vec3<f32>) -> vec3<f32> {
    let c = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    let cutoff = c < vec3<f32>(0.0031308);
    let higher = vec3<f32>(1.055) * pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    let lower = c * vec3<f32>(12.92);
    return select(higher, lower, cutoff);
}

fn linear_to_oklab(color: vec4<f32>) -> vec4<f32> {
    let l = max(0.4122214708 * color.r + 0.5363325363 * color.g + 0.0514459929 * color.b, 0.0);
    let m = max(0.2119034982 * color.r + 0.6806995451 * color.g + 0.1073969566 * color.b, 0.0);
    let s = max(0.0883024619 * color.r + 0.2817188376 * color.g + 0.6299787005 * color.b, 0.0);
    let l_ = pow(l, 1.0 / 3.0);
    let m_ = pow(m, 1.0 / 3.0);
    let s_ = pow(s, 1.0 / 3.0);
    return vec4<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
        color.a
    );
}

fn oklab_to_linear(color: vec4<f32>) -> vec4<f32> {
    let l_ = color.r + 0.3963377774 * color.g + 0.2158037573 * color.b;
    let m_ = color.r - 0.1055613458 * color.g - 0.0638541728 * color.b;
    let s_ = color.r - 0.0894841775 * color.g - 1.2914855480 * color.b;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec4<f32>(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
        color.a
    );
}

fn eval_tint(pixel_pos: vec2<f32>) -> vec4<f32> {
    if (comp.viewport_and_flags.w < 0.5) {
        return comp.tint_color0;
    }
    let angle = comp.uv_scale_and_grad.z;
    let radians = (angle % 360.0 - 90.0) * 3.1415926 / 180.0;
    var direction = vec2<f32>(cos(radians), sin(radians));
    let sz = comp.bounds.zw;
    if (sz.x > sz.y) {
        direction.y *= sz.y / sz.x;
    } else {
        direction.x *= sz.x / sz.y;
    }
    let half_size = sz * 0.5;
    let center = comp.bounds.xy + half_size;
    var t = dot(pixel_pos - center, direction) / length(direction);
    if (abs(direction.x) > abs(direction.y)) {
        t = (t + half_size.x) / sz.x;
    } else {
        t = (t + half_size.y) / sz.y;
    }
    let stop0 = comp.tint_stops.x;
    let stop1 = comp.tint_stops.y;
    t = clamp((t - stop0) / max(stop1 - stop0, 1e-5), 0.0, 1.0);
    if (comp.uv_scale_and_grad.w > 0.5) {
        let ok0 = linear_to_oklab(vec4<f32>(srgb_to_linear_3(comp.tint_color0.rgb), comp.tint_color0.a));
        let ok1 = linear_to_oklab(vec4<f32>(srgb_to_linear_3(comp.tint_color1.rgb), comp.tint_color1.a));
        let lin = oklab_to_linear(mix(ok0, ok1, t));
        return vec4<f32>(linear_to_srgb_3(lin.rgb), lin.a);
    }
    return mix(comp.tint_color0, comp.tint_color1, t);
}

@fragment
fn fs_composite(in: CompositeOut) -> @location(0) vec4<f32> {
    // Content-mask clip.
    let tl = in.pixel_pos - comp.content_mask.xy;
    let br = comp.content_mask.xy + comp.content_mask.zw - in.pixel_pos;
    if (any(vec4<f32>(tl.x, br.x, tl.y, br.y) < vec4<f32>(0.0))) {
        return vec4<f32>(0.0);
    }

    // Rounded-rectangle SDF matching GPUI's `quad_sdf_impl`.
    let half_size = comp.bounds.zw * 0.5;
    let center_to_point = in.pixel_pos - (comp.bounds.xy + half_size);
    var corner_radius = comp.corner_radii.x;
    if (center_to_point.x < 0.0) {
        if (center_to_point.y < 0.0) {
            corner_radius = comp.corner_radii.x;
        } else {
            corner_radius = comp.corner_radii.w;
        }
    } else {
        if (center_to_point.y < 0.0) {
            corner_radius = comp.corner_radii.y;
        } else {
            corner_radius = comp.corner_radii.z;
        }
    }
    let q = abs(center_to_point) - half_size + corner_radius;
    let outer_sdf = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - corner_radius;
    let edge_alpha = saturate(0.5 - outer_sdf);
    if (edge_alpha <= 0.0) {
        return vec4<f32>(0.0);
    }

    // Sample quarter-res Dual-Kawase blurred backdrop.
    let local_uv = (in.pixel_pos - comp.src_rect.xy) / comp.src_rect.zw;
    let sample_uv = clamp(local_uv * comp.uv_scale_and_grad.xy, comp.uv_clamp.xy, comp.uv_clamp.zw);
    let blurred = textureSample(t_src, s_src, sample_uv);

    // Blend capsule glass tint over blurred backdrop.
    var tint = eval_tint(in.pixel_pos);
    tint.a = clamp(tint.a * comp.tint_stops.z, 0.0, 1.0);
    var rgb = mix(blurred.rgb, tint.rgb, tint.a);

    // 1/255 triangular-PDF Void-and-Cluster blue-noise dither eliminates 8-bit banding.
    let px_coord = vec2<u32>(max(in.pixel_pos, vec2<f32>(0.0)));
    let n1 = sample_blue_noise(px_coord);
    let n2 = sample_blue_noise(px_coord + vec2<u32>(7u, 11u));
    let dither = (n1 - n2) / 255.0;
    rgb = clamp(rgb + vec3<f32>(dither), vec3<f32>(0.0), vec3<f32>(1.0));

    let mult = select(1.0, edge_alpha, comp.viewport_and_flags.z > 0.5);
    return vec4<f32>(rgb * mult, edge_alpha);
}
"#;

#[cfg(test)]
mod tests {
    use super::BACKDROP_BLUR_SHADER;

    #[test]
    fn backdrop_blur_shader_is_valid_wgsl() {
        let module = wgpu::naga::front::wgsl::parse_str(BACKDROP_BLUR_SHADER)
            .expect("backdrop blur shader should parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("backdrop blur shader should validate");
    }
}
