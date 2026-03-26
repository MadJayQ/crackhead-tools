/// `MapPipeline` — a single wgpu render pipeline used for both OSM base-map
/// tiles and image overlays (Sentinel-2, NEXRAD).
///
/// Pipeline layout
/// ───────────────
///   Vertex buffer   │ TileVertex { position: vec2, uv: vec2 }
///   group(0) bind   │ texture_2d + sampler          (per draw call)
///   group(1) bind   │ uniform Params { opacity: f32 } (per draw call)
///
/// Each tile/overlay is drawn as an indexed quad (2 triangles, 6 indices).
/// Alpha blending is enabled so overlays appear transparent over the base map.
///
/// WGSL shader is embedded as a string constant below.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

use super::context::GpuContext;

// ── Vertex ────────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct TileVertex {
    /// NDC position: x ∈ [-1, 1], y ∈ [-1, 1].
    pub position: [f32; 2],
    /// Texture UV: (0,0) = top-left, (1,1) = bottom-right.
    pub uv: [f32; 2],
}

impl TileVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
    ];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TileVertex>() as u64,
            step_mode:    wgpu::VertexStepMode::Vertex,
            attributes:   &Self::ATTRIBS,
        }
    }
}

/// Build the 4 vertices + 6 indices for a textured quad.
///
/// `corners` is `[TL, TR, BL, BR]` as NDC positions.
pub fn quad_vertices(corners: [[f32; 2]; 4]) -> ([TileVertex; 4], [u16; 6]) {
    let uvs = [
        [0.0f32, 0.0], // TL
        [1.0,    0.0], // TR
        [0.0,    1.0], // BL
        [1.0,    1.0], // BR
    ];
    let verts = std::array::from_fn(|i| TileVertex {
        position: corners[i],
        uv:       uvs[i],
    });
    // Two triangles: TL-TR-BL and TR-BR-BL
    let idx: [u16; 6] = [0, 1, 2, 1, 3, 2];
    (verts, idx)
}

// ── Uniforms ──────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct DrawParams {
    /// Alpha multiplier applied in the fragment shader.
    pub opacity: f32,
    /// Pad to 16-byte alignment required by wgpu uniform buffers.
    pub _pad: [f32; 3],
}

// ── Bind-group layouts ────────────────────────────────────────────────────────

pub struct MapPipeline {
    pub pipeline:          wgpu::RenderPipeline,
    pub texture_bgl:       wgpu::BindGroupLayout,  // group(0): tex + sampler
    pub params_bgl:        wgpu::BindGroupLayout,  // group(1): uniform
    pub default_sampler:   wgpu::Sampler,
}

impl MapPipeline {
    pub fn new(gpu: &GpuContext) -> Self {
        let dev = &gpu.device;

        // ── Bind-group layouts ──────────────────────────────────────────────────
        let texture_bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tile texture bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled:   false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let params_bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tile params bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding:    0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty:                 wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size:   None,
                },
                count: None,
            }],
        });

        // ── Pipeline layout ─────────────────────────────────────────────────────
        let pipeline_layout =
            dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label:                Some("map pipeline layout"),
                bind_group_layouts:   &[&texture_bgl, &params_bgl],
                push_constant_ranges: &[],
            });

        // ── Shader ──────────────────────────────────────────────────────────────
        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:  Some("tile shader"),
            source: wgpu::ShaderSource::Wgsl(TILE_WGSL.into()),
        });

        // ── Alpha-blend state ───────────────────────────────────────────────────
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation:  wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation:  wgpu::BlendOperation::Add,
            },
        };

        // ── Render pipeline ─────────────────────────────────────────────────────
        let pipeline = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:  Some("map pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module:               &shader,
                entry_point:          Some("vs_main"),
                buffers:              &[TileVertex::layout()],
                compilation_options:  Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module:              &shader,
                entry_point:         Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format:     gpu.surface_format,
                    blend:      Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology:  wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil:  None,
            multisample:    wgpu::MultisampleState::default(),
            multiview:      None,
            cache:          None,
        });

        let default_sampler = dev.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter:     wgpu::FilterMode::Linear,
            min_filter:     wgpu::FilterMode::Linear,
            mipmap_filter:  wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            texture_bgl,
            params_bgl,
            default_sampler,
        }
    }

    // ── Per-draw helpers ───────────────────────────────────────────────────────

    /// Upload RGBA pixels as a new wgpu texture.
    pub fn upload_texture(
        &self,
        gpu: &GpuContext,
        label: &str,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label:           Some(label),
            size,
            mip_level_count: 1,
            sample_count:    1,
            dimension:       wgpu::TextureDimension::D2,
            // Tiles are PNG-decoded RGBA8; use Unorm (linear) here because
            // the source images already contain display-ready sRGB data.
            format:          wgpu::TextureFormat::Rgba8UnormSrgb,
            usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats:    &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture:   &texture,
                mip_level: 0,
                origin:    wgpu::Origin3d::ZERO,
                aspect:    wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset:         0,
                bytes_per_row:  Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );
        texture
    }

    /// Create a bind group for a texture view + the shared sampler.
    pub fn texture_bind_group(
        &self,
        gpu: &GpuContext,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("tile texture bg"),
            layout:  &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding:  0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding:  1,
                    resource: wgpu::BindingResource::Sampler(&self.default_sampler),
                },
            ],
        })
    }

    /// Create a params bind group for the given opacity value.
    pub fn params_bind_group(&self, gpu: &GpuContext, opacity: f32) -> wgpu::BindGroup {
        let buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("tile params buf"),
            contents: bytemuck::bytes_of(&DrawParams { opacity, _pad: [0.0; 3] }),
            usage:    wgpu::BufferUsages::UNIFORM,
        });
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("tile params bg"),
            layout:  &self.params_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding:  0,
                resource: buf.as_entire_binding(),
            }],
        })
    }

    /// Draw one textured quad to `render_pass`.
    ///
    /// `corners` — NDC corners `[TL, TR, BL, BR]`.
    pub fn draw_quad(
        &self,
        gpu: &GpuContext,
        render_pass: &mut wgpu::RenderPass<'_>,
        corners: [[f32; 2]; 4],
        texture_bg: &wgpu::BindGroup,
        params_bg: &wgpu::BindGroup,
    ) {
        let (verts, idx) = quad_vertices(corners);

        let vbuf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("quad vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage:    wgpu::BufferUsages::VERTEX,
        });
        let ibuf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("quad ibuf"),
            contents: bytemuck::cast_slice(&idx),
            usage:    wgpu::BufferUsages::INDEX,
        });

        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, texture_bg, &[]);
        render_pass.set_bind_group(1, params_bg, &[]);
        render_pass.set_vertex_buffer(0, vbuf.slice(..));
        render_pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint16);
        render_pass.draw_indexed(0..6, 0, 0..1);
    }
}

// ── WGSL shader ───────────────────────────────────────────────────────────────

const TILE_WGSL: &str = r#"
// ── Vertex stage ──────────────────────────────────────────────────────────────

struct VertexIn {
    @location(0) position : vec2<f32>,
    @location(1) uv       : vec2<f32>,
};

struct VertexOut {
    @builtin(position) clip : vec4<f32>,
    @location(0)       uv   : vec2<f32>,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    return VertexOut(vec4<f32>(in.position, 0.0, 1.0), in.uv);
}

// ── Fragment stage ─────────────────────────────────────────────────────────────

@group(0) @binding(0) var t_tile : texture_2d<f32>;
@group(0) @binding(1) var s_tile : sampler;

struct Params { opacity: f32 };
@group(1) @binding(0) var<uniform> params : Params;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_tile, s_tile, in.uv);
    // Pre-multiply alpha so blending works correctly when drawing on top of
    // already-composited pixels.
    return vec4<f32>(c.rgb * c.a * params.opacity, c.a * params.opacity);
}
"#;
