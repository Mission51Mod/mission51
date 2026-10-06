//! The 3-D terrain view on eframe's own wgpu device (no second device): the heightfield mesh and the navmesh lines
//! are drawn into an offscreen colour + depth target (4x MSAA, resolved), which egui shows as an ordinary image.
//! Rendering happens on the UI thread, only when the camera, the size or the data changed; the submission is queued
//! before egui's own, so the image is current in the same frame. Callers skip it entirely while a game runs.
use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;

/// terrain vertex: position (world metres) + normal
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TerrainVertex {
    pub pos: [f32; 3],
    pub nrm: [f32; 3],
}

/// model vertex: source position, normal and UV0 (UV is not generated from world position).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelVertex {
    pub pos: [f32; 3],
    pub nrm: [f32; 3],
    pub uv: [f32; 2],
}

/// line vertex: position + colour (linear RGBA)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

fn bytes<T>(v: &[T]) -> &[u8] {
    // plain #[repr(C)] f32 / u32 data, no padding
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    /// xyz: direction towards the light, w: ambient
    light: [f32; 4],
    /// min height, max height, water level (or -1e9), vertical exaggeration
    hrange: [f32; 4],
    /// camera position xyz, fog distance
    eye: [f32; 4],
    /// background colour (fog)
    fog: [f32; 4],
    /// world texture placement: x0, z0, 1 / width, 1 / depth (metres)
    tex_rect: [f32; 4],
    /// x: world texture on (1) / off (0)
    opts: [f32; 4],
}

const SHADER: &str = r#"
struct U {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
    hrange: vec4<f32>,
    eye: vec4<f32>,
    fog: vec4<f32>,
    tex_rect: vec4<f32>,
    opts: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var wt: texture_2d<f32>;
@group(0) @binding(2) var wt_s: sampler;
struct Material { flags: vec4<f32> };
@group(0) @binding(3) var<uniform> material: Material;

struct TOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) nrm: vec3<f32>,
    @location(1) h: f32,
    @location(2) wpos: vec3<f32>,
};

@vertex
fn vs_terrain(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>) -> TOut {
    var o: TOut;
    let p = vec3<f32>(pos.x, pos.y * u.hrange.w, pos.z);
    o.clip = u.view_proj * vec4<f32>(p, 1.0);
    o.nrm = normalize(vec3<f32>(nrm.x, nrm.y / max(u.hrange.w, 0.01), nrm.z));
    o.h = pos.y;
    o.wpos = p;
    return o;
}

fn ramp(t: f32) -> vec3<f32> {
    // shore sand -> lowland green -> upland olive -> rock -> pale summit
    let c0 = vec3<f32>(0.76, 0.70, 0.52);
    let c1 = vec3<f32>(0.30, 0.47, 0.22);
    let c2 = vec3<f32>(0.47, 0.50, 0.30);
    let c3 = vec3<f32>(0.50, 0.46, 0.40);
    let c4 = vec3<f32>(0.85, 0.84, 0.80);
    if (t < 0.08) { return mix(c0, c1, t / 0.08); }
    if (t < 0.45) { return mix(c1, c2, (t - 0.08) / 0.37); }
    if (t < 0.80) { return mix(c2, c3, (t - 0.45) / 0.35); }
    return mix(c3, c4, clamp((t - 0.80) / 0.20, 0.0, 1.0));
}

@fragment
fn fs_terrain(i: TOut) -> @location(0) vec4<f32> {
    let n = normalize(i.nrm);
    let t = clamp((i.h - u.hrange.x) / max(u.hrange.y - u.hrange.x, 0.001), 0.0, 1.0);
    let uv = vec2<f32>((i.wpos.x - u.tex_rect.x) * u.tex_rect.z, (i.wpos.z - u.tex_rect.y) * u.tex_rect.w);
    let tex = textureSample(wt, wt_s, uv).rgb;
    var base = ramp(t);
    let slope = 1.0 - n.y;
    base = mix(base, vec3<f32>(0.42, 0.40, 0.37), smoothstep(0.25, 0.55, slope));
    if (u.opts.x > 0.5) {
        base = tex * 1.2;
    }
    let model = u.opts.y > 0.5;
    if (model) {
        base = vec3<f32>(0.70, 0.71, 0.74);
    }
    if (i.h < u.hrange.z && !model) {
        let depth = clamp((u.hrange.z - i.h) / 12.0, 0.0, 1.0);
        base = mix(vec3<f32>(0.20, 0.42, 0.50), vec3<f32>(0.05, 0.18, 0.30), depth);
    }
    let l = normalize(u.light.xyz);
    // models are drawn two-sided (no culling): light the side facing the camera
    let diff = select(max(dot(n, l), 0.0), abs(dot(n, l)), model);
    var c = base * (u.light.w + (1.0 - u.light.w) * diff);
    let d = distance(i.wpos, u.eye.xyz);
    let f = clamp(d / u.eye.w, 0.0, 1.0);
    c = mix(c, u.fog.rgb, f * f * 0.6);
    return vec4<f32>(c, 1.0);
}

struct MOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) nrm: vec3<f32>,
    @location(1) uv: vec2<f32>,
};
@vertex
fn vs_model(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>, @location(2) uv: vec2<f32>) -> MOut {
    var o: MOut;
    o.clip = u.view_proj * vec4<f32>(pos, 1.0);
    o.nrm = nrm;
    o.uv = uv;
    return o;
}
@fragment
fn fs_model(i: MOut) -> @location(0) vec4<f32> {
    let n = normalize(i.nrm);
    // Source FMDL V grows upward; decoded FTEX row zero is the top. Keep wrap for seam UVs.
    let tex = textureSample(wt, wt_s, vec2<f32>(i.uv.x, 1.0 - i.uv.y));
    var base = vec3<f32>(0.70, 0.71, 0.74);
    if (u.opts.z > 0.5 && material.flags.x > 0.5) {
        base = tex.rgb;
        if (material.flags.y > 0.5 && tex.a < 0.5) { discard; }
    }
    if (u.opts.w > 1.5) { return vec4<f32>(n * 0.5 + 0.5, 1.0); }
    if (u.opts.w > 0.5) {
        if (material.flags.z < 0.5) { return vec4<f32>(0.5, 0.15, 0.5, 1.0); }
        let cell = floor(i.uv * 16.0);
        let checker = (i32(cell.x) + i32(cell.y)) & 1;
        base = select(vec3<f32>(0.08, 0.15, 0.23), vec3<f32>(0.65, 0.82, 0.94), checker == 0);
    }
    let diff = abs(dot(n, normalize(u.light.xyz)));
    return vec4<f32>(base * (u.light.w + (1.0 - u.light.w) * diff), 1.0);
}

struct LOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(@location(0) pos: vec3<f32>, @location(1) color: vec4<f32>) -> LOut {
    var o: LOut;
    o.clip = u.view_proj * vec4<f32>(pos.x, pos.y * u.hrange.w, pos.z, 1.0);
    // pull lines a little towards the camera so they sit on the surface they follow
    o.clip.z = o.clip.z - 0.0004 * o.clip.w;
    o.color = color;
    return o;
}

@fragment
fn fs_line(i: LOut) -> @location(0) vec4<f32> {
    return i.color;
}
"#;

/// Orbit camera around a target point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: [f32; 3],
    /// radians, around +Y
    pub yaw: f32,
    /// radians above the horizon
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
}

impl Camera {
    pub fn eye(&self) -> [f32; 3] {
        let (cp, sp) = (self.pitch.cos(), self.pitch.sin());
        let (cy, sy) = (self.yaw.cos(), self.yaw.sin());
        [self.target[0] + self.distance * cp * sy, self.target[1] + self.distance * sp, self.target[2] + self.distance * cp * cy]
    }

    pub fn view_proj(&self, aspect: f32) -> [[f32; 4]; 4] {
        let eye = self.eye();
        let near = (self.distance * 0.002).max(0.002);
        let far = self.distance * 10.0 + 100.0;
        mul(&perspective(self.fov_y, aspect, near, far), &look_at(eye, self.target, [0.0, 1.0, 0.0]))
    }
}

// column-major 4x4 helpers (wgpu / WGSL convention: m[col][row]), depth 0..1
fn look_at(eye: [f32; 3], at: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = norm3(sub3(at, eye));
    let s = norm3(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot3(s, eye), -dot3(u, eye), dot3(f, eye), 1.0],
    ]
}

fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let t = 1.0 / (fov_y / 2.0).tan();
    let r = far / (near - far);
    [[t / aspect, 0.0, 0.0, 0.0], [0.0, t, 0.0, 0.0], [0.0, 0.0, r, -1.0], [0.0, 0.0, r * near, 0.0]]
}

fn mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut m = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            m[c][r] = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    m
}

pub fn transform(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 4] {
    let mut o = [0.0f32; 4];
    for (r, v) in o.iter_mut().enumerate() {
        *v = m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r];
    }
    o
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm3(a: [f32; 3]) -> [f32; 3] {
    let l = dot3(a, a).sqrt().max(1e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// how a frame looks
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub hmin: f32,
    pub hmax: f32,
    pub water: Option<f32>,
    pub exaggeration: f32,
    /// background (sRGB 0..1)
    pub background: [f32; 3],
    pub show_lines: bool,
    /// drape the world texture (when one is set)
    pub world_texture: bool,
    /// neutral model shading (two-sided, no height ramp, no water)
    pub model: bool,
    /// model albedo on/off; inspection mode: 0 shaded, 1 UV checker, 2 normal colours
    pub model_textures: bool,
    pub model_inspection: u8,
    /// light azimuth / elevation (radians)
    pub sun_azimuth: f32,
    pub sun_elevation: f32,
}

struct Target {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    _resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    depth: wgpu::TextureView,
}

struct ModelDraw {
    mesh: usize,
    indices: std::ops::Range<u32>,
    binding: wgpu::BindGroup,
}

/// GPU state of the 3-D terrain view (one per view).
pub struct TerrainGpu {
    rs: egui_wgpu::RenderState,
    terrain_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    model_pipeline: wgpu::RenderPipeline,
    bone_pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    world: Option<(f32, f32, f32, f32)>,
    mesh: Option<(wgpu::Buffer, wgpu::Buffer, u32)>,
    lines: Option<(wgpu::Buffer, u32)>,
    bone_lines: Option<(wgpu::Buffer, u32)>,
    model_draws: Vec<ModelDraw>,
    visible_mesh: Option<usize>,
    target: Option<Target>,
    texture_id: Option<egui::TextureId>,
    last: Option<([u32; 2], Camera, Look, u64)>,
    generation: u64,
}

impl TerrainGpu {
    pub fn new(rs: &egui_wgpu::RenderState) -> TerrainGpu {
        let device = &rs.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fox-studio terrain"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fox-studio terrain uniforms"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fox-studio terrain layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fox-studio terrain uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world texture"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let white = upload_texture(device, &rs.queue, &[vec![255u8; 4]], 1);
        let bind_group = make_bind_group(device, &bgl, &uniforms, &white, &sampler);
        let pipeline = |label: &str, vs: &str, fs: &str, topology: wgpu::PrimitiveTopology, attrs: &[wgpu::VertexAttribute], stride: u64, write_depth: bool, xray: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    buffers: &[Some(wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Vertex, attributes: attrs })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(write_depth),
                    depth_compare: Some(if xray { wgpu::CompareFunction::Always } else { wgpu::CompareFunction::LessEqual }),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState { count: SAMPLES, mask: !0, alpha_to_coverage_enabled: false },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let terrain_pipeline = pipeline("fox-studio terrain", "vs_terrain", "fs_terrain", wgpu::PrimitiveTopology::TriangleList,
                                        &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3], 24, true, false);
        let line_pipeline = pipeline("fox-studio lines", "vs_line", "fs_line", wgpu::PrimitiveTopology::LineList,
                                     &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4], 28, false, false);
        let model_pipeline = pipeline("fox-studio model", "vs_model", "fs_model", wgpu::PrimitiveTopology::TriangleList,
            &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2], 32, true, false);
        let bone_pipeline = pipeline("fox-studio bind bones", "vs_line", "fs_line", wgpu::PrimitiveTopology::LineList,
            &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4], 28, false, true);
        TerrainGpu { rs: rs.clone(), terrain_pipeline, line_pipeline, model_pipeline, bone_pipeline, uniforms, bind_group, bgl, sampler, world: None, mesh: None,
                     lines: None, bone_lines: None, model_draws: vec![], visible_mesh: None, target: None, texture_id: None, last: None, generation: 0 }
    }

    /// The world texture (RGBA, row 0 = smallest z) covering `rect` = (x0, z0, x1, z1); None removes it.
    pub fn set_world_texture(&mut self, img: Option<&super::texture::Image>, rect: (f32, f32, f32, f32)) {
        let d = &self.rs.device;
        match img {
            Some(img) => {
                let (side, levels) = mip_chain(img);
                let tex = upload_texture(d, &self.rs.queue, &levels, side as u32);
                self.bind_group = make_bind_group(d, &self.bgl, &self.uniforms, &tex, &self.sampler);
                self.world = Some(rect);
            }
            None => {
                let white = upload_texture(d, &self.rs.queue, &[vec![255u8; 4]], 1);
                self.bind_group = make_bind_group(d, &self.bgl, &self.uniforms, &white, &self.sampler);
                self.world = None;
            }
        }
        self.generation += 1;
    }

    pub fn has_world_texture(&self) -> bool {
        self.world.is_some()
    }

    /// a CPU (software) adapter such as WARP / llvmpipe: draw lighter meshes
    pub fn is_software(&self) -> bool {
        self.rs.adapter.get_info().device_type == wgpu::DeviceType::Cpu
    }

    pub fn adapter_name(&self) -> String {
        let i = self.rs.adapter.get_info();
        format!("{} ({:?})", i.name, i.backend)
    }

    pub fn set_mesh(&mut self, verts: &[TerrainVertex], indices: &[u32]) {
        let d = &self.rs.device;
        let vb = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("terrain vertices"), contents: bytes(verts), usage: wgpu::BufferUsages::VERTEX });
        let ib = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("terrain indices"), contents: bytes(indices), usage: wgpu::BufferUsages::INDEX });
        self.mesh = Some((vb, ib, indices.len() as u32));
        self.generation += 1;
    }

    /// CPU decoding/mip preparation is complete before this bounded upload; no texture is resized to a square.
    pub fn set_model(&mut self, model: &super::model::Model) {
        let device = &self.rs.device;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("model vertices"),
            contents: bytes(&model.verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("model triangles"),
            contents: bytes(&model.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        self.mesh = Some((vertex_buffer, index_buffer, model.indices.len() as u32));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model wrap sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let mut textures = std::collections::HashMap::new();
        for material in model.unique_texture_materials() {
            if let Some(levels) = &material.image {
                let key = (std::sync::Arc::as_ptr(levels) as usize, material.linear);
                textures.insert(
                    key,
                    upload_model_texture(device, &self.rs.queue, levels, material.linear),
                );
            }
        }
        let white = upload_texture(device, &self.rs.queue, &[vec![255u8; 4]], 1);
        self.model_draws = model
            .meshes
            .iter()
            .map(|mesh| {
                let texture = model.materials.get(mesh.material).and_then(|mat| {
                    mat.image
                        .as_ref()
                        .and_then(|img| textures.get(&(std::sync::Arc::as_ptr(img) as usize, mat.linear)))
                });
                let flags = [
                    if texture.is_some() && mesh.has_uv { 1.0 } else { 0.0 },
                    if mesh.alpha_mode == 0x20 { 1.0 } else { 0.0 },
                    if mesh.has_uv { 1.0 } else { 0.0 },
                    0.0,
                ];
                ModelDraw {
                    mesh: mesh.id,
                    indices: mesh.indices.clone(),
                    binding: make_material_binding(
                        device,
                        &self.bgl,
                        &self.uniforms,
                        texture.unwrap_or(&white),
                        &sampler,
                        flags,
                    ),
                }
            })
            .collect();
        self.visible_mesh = None;
        self.generation += 1;
    }

    pub fn set_visible_mesh(&mut self, mesh: Option<usize>) {
        if self.visible_mesh != mesh {
            self.visible_mesh = mesh;
            self.generation += 1;
        }
    }

    pub fn set_bone_lines(&mut self, verts: &[LineVertex]) {
        self.bone_lines = if verts.is_empty() {
            None
        } else {
            let b = self.rs.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bind-pose bones"),
                contents: bytes(verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            Some((b, verts.len() as u32))
        };
        self.generation += 1;
    }
    pub fn set_lines(&mut self, verts: &[LineVertex]) {
        self.lines = if verts.is_empty() {
            None
        } else {
            let b = self.rs.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("nav lines"), contents: bytes(verts), usage: wgpu::BufferUsages::VERTEX });
            Some((b, verts.len() as u32))
        };
        self.generation += 1;
    }

    pub fn has_mesh(&self) -> bool {
        self.mesh.is_some()
    }

    fn ensure_target(&mut self, size: [u32; 2]) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let d = &self.rs.device;
        let ext = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
        let tex = |label: &str, format, samples, usage: wgpu::TextureUsages| {
            let view_formats: &[wgpu::TextureFormat] = if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
                &[wgpu::TextureFormat::Rgba8Unorm]
            } else {
                &[]
            };
            d.create_texture(&wgpu::TextureDescriptor { label: Some(label), size: ext, mip_level_count: 1, sample_count: samples,
                dimension: wgpu::TextureDimension::D2, format, usage, view_formats })
        };
        let msaa = tex("terrain msaa", COLOR_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let resolve = tex("terrain colour", COLOR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let depth = tex("terrain depth", DEPTH_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let resolve_view = resolve.create_view(&Default::default());
        // Render in linear light into the sRGB target. egui 0.36 expects encoded bytes in an Unorm view;
        // giving it the sRGB view decodes those bytes again and visibly darkens the entire preview.
        let egui_view = resolve.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm), ..Default::default()
        });
        {
            let mut r = self.rs.renderer.write();
            match self.texture_id {
                Some(id) => r.update_egui_texture_from_wgpu_texture(d, &egui_view, wgpu::FilterMode::Linear, id),
                None => self.texture_id = Some(r.register_native_texture(d, &egui_view, wgpu::FilterMode::Linear)),
            }
        }
        self.target = Some(Target {
            size,
            msaa: msaa.create_view(&Default::default()),
            _resolve: resolve,
            resolve_view,
            depth: depth.create_view(&Default::default()),
        });
        self.last = None;
    }

    /// Render if anything changed; returns the image to show.
    pub fn render(&mut self, size: [u32; 2], cam: &Camera, look: &Look) -> Option<egui::TextureId> {
        let size = [size[0].clamp(16, 4096), size[1].clamp(16, 4096)];
        self.ensure_target(size);
        let key = (size, *cam, *look, self.generation);
        if self.last.as_ref() == Some(&key) {
            return self.texture_id;
        }
        let Some(t) = &self.target else { return None };
        let aspect = size[0] as f32 / size[1] as f32;
        let eye = cam.eye();
        let (ca, sa) = (look.sun_azimuth.cos(), look.sun_azimuth.sin());
        let ce = look.sun_elevation.cos();
        let bg = look.background.map(srgb_to_linear);
        let u = Uniforms {
            view_proj: cam.view_proj(aspect),
            light: [ce * sa, look.sun_elevation.sin(), ce * ca, 0.32],
            hrange: [look.hmin, look.hmax, look.water.unwrap_or(-1e9), look.exaggeration],
            eye: [eye[0], eye[1], eye[2], if look.model { 1e9 } else { cam.distance * 4.0 + 2000.0 }],
            fog: [bg[0], bg[1], bg[2], 1.0],
            tex_rect: match self.world {
                Some((x0, z0, x1, z1)) => [x0, z0, 1.0 / (x1 - x0).max(1e-3), 1.0 / (z1 - z0).max(1e-3)],
                None => [0.0, 0.0, 1.0, 1.0],
            },
            opts: [if look.world_texture && self.world.is_some() { 1.0 } else { 0.0 }, if look.model { 1.0 } else { 0.0 },
                if look.model_textures { 1.0 } else { 0.0 }, look.model_inspection as f32],
        };
        let q = &self.rs.queue;
        q.write_buffer(&self.uniforms, 0, bytes(std::slice::from_ref(&u)));
        let mut enc = self.rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fox-studio terrain") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fox-studio terrain"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.msaa,
                    resolve_target: Some(&t.resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: bg[0] as f64, g: bg[1] as f64, b: bg[2] as f64, a: 1.0 }),
                        store: wgpu::StoreOp::Discard,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            if let Some((vb, ib, n)) = &self.mesh {
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                if look.model && !self.model_draws.is_empty() {
                    pass.set_pipeline(&self.model_pipeline);
                    for draw in &self.model_draws {
                        if self.visible_mesh.is_some_and(|id| id != draw.mesh) { continue; }
                        pass.set_bind_group(0, &draw.binding, &[]);
                        pass.draw_indexed(draw.indices.clone(), 0, 0..1);
                    }
                    pass.set_bind_group(0, &self.bind_group, &[]);
                } else {
                    pass.set_pipeline(&self.terrain_pipeline);
                    pass.draw_indexed(0..*n, 0, 0..1);
                }
            }
            if look.show_lines && let Some((line_buffer, line_count)) = &self.lines {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, line_buffer.slice(..));
                pass.draw(0..*line_count, 0..1);
            }
            if let Some((lb, n)) = &self.bone_lines {
                pass.set_pipeline(&self.bone_pipeline);
                pass.set_vertex_buffer(0, lb.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        q.submit(Some(enc.finish()));
        self.last = Some(key);
        self.texture_id
    }
}

impl Drop for TerrainGpu {
    fn drop(&mut self) {
        if let Some(id) = self.texture_id.take() {
            self.rs.renderer.write().free_texture(&id);
        }
    }
}

fn make_bind_group(d: &wgpu::Device, bgl: &wgpu::BindGroupLayout, u: &wgpu::Buffer, tex: &wgpu::Texture, s: &wgpu::Sampler) -> wgpu::BindGroup {
    make_material_binding(d, bgl, u, tex, s, [0.0; 4])
}

fn make_material_binding(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    texture: &wgpu::Texture,
    sampler: &wgpu::Sampler,
    flags: [f32; 4],
) -> wgpu::BindGroup {
    let view = texture.create_view(&Default::default());
    let material = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("model material flags"),
        contents: bytes(&flags),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fox-studio terrain"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: material.as_entire_binding(),
            },
        ],
    })
}

fn upload_model_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    levels: &[super::texture::Image],
    linear: bool,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model albedo"),
        size: wgpu::Extent3d {
            width: levels[0].width as u32,
            height: levels[0].height as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if linear {
            wgpu::TextureFormat::Rgba8Unorm
        } else {
            wgpu::TextureFormat::Rgba8UnormSrgb
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (i, img) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: i as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width as u32 * 4),
                rows_per_image: Some(img.height as u32),
            },
            wgpu::Extent3d {
                width: img.width as u32,
                height: img.height as u32,
                depth_or_array_layers: 1,
            },
        );
    }
    texture
}

/// a square sRGB texture with the given mip levels (level 0 = `side` x `side`)
fn upload_texture(d: &wgpu::Device, q: &wgpu::Queue, levels: &[Vec<u8>], side: u32) -> wgpu::Texture {
    let tex = d.create_texture(&wgpu::TextureDescriptor {
        label: Some("world texture"),
        size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (l, data) in levels.iter().enumerate() {
        let s = (side >> l).max(1);
        q.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: l as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * s), rows_per_image: Some(s) },
            wgpu::Extent3d { width: s, height: s, depth_or_array_layers: 1 },
        );
    }
    tex
}

/// (side, levels): the image resized to a power-of-two square (at most 4096), then 2 x 2 box-filtered down to 1 x 1
pub fn mip_chain(img: &super::texture::Image) -> (usize, Vec<Vec<u8>>) {
    let side = img.width.max(img.height).next_power_of_two().min(4096);
    let base = super::worldtex::resize(img, side);
    let mut levels = vec![base.rgba];
    let mut s = side;
    while s > 1 {
        let prev = levels.last().unwrap();
        let n = s / 2;
        let mut out = vec![0u8; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                for c in 0..4 {
                    let at = |xx: usize, yy: usize| prev[4 * (yy * s + xx) + c] as u32;
                    out[4 * (y * n + x) + c] = ((at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1)) / 4) as u8;
                }
            }
        }
        levels.push(out);
        s = n;
    }
    (side, levels)
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_sizes() {
        let img = super::super::texture::Image { width: 6, height: 6, rgba: vec![200; 6 * 6 * 4] };
        let (side, l) = mip_chain(&img);
        assert_eq!(side, 8);
        assert_eq!(l.iter().map(|x| x.len()).collect::<Vec<_>>(), vec![256, 64, 16, 4]);
        assert!(l[3].iter().all(|&v| v == 200));
    }

    #[test]
    fn camera_projects_target_to_centre() {
        let cam = Camera { target: [100.0, 20.0, -50.0], yaw: 0.7, pitch: 0.5, distance: 300.0, fov_y: 0.8 };
        let m = cam.view_proj(1.5);
        let c = transform(&m, cam.target);
        assert!((c[0] / c[3]).abs() < 1e-4 && (c[1] / c[3]).abs() < 1e-4);
        let z = c[2] / c[3];
        assert!(z > 0.0 && z < 1.0, "depth {z}");
        // a point above the target is above the centre on screen
        let up = transform(&m, [100.0, 40.0, -50.0]);
        assert!(up[1] / up[3] > 0.0);
        let e = cam.eye();
        let d = ((e[0] - 100.0).powi(2) + (e[1] - 20.0).powi(2) + (e[2] + 50.0).powi(2)).sqrt();
        assert!((d - 300.0).abs() < 1e-2);
    }
}
