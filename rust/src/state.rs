// GPU state + virtualized, streamed wall.
//
// This is the bounded-memory core. We never hold the whole library on the GPU: a fixed pool of
// texture-array layers is recycled as tiles scroll in and out of a window around the camera.
// Decoding runs on a worker thread pool (files read directly — no IPC, no protocol), and the
// main thread only assigns a free layer + uploads when pixels come back. Scroll a 16k-photo
// library and GPU/CPU stay flat — by construction, not by fighting a garbage collector.

use std::collections::HashMap;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

// Layout + motion tuned to match the web wall (libmpv/src/wall/WallScene.ts).
const TILE_PX: u32 = 512; // texture-array layer size (images resized to fit, preserving aspect)
const ROWS: usize = 3;
const TILE: f32 = 1.0; // row height (ROW_H)
const MAX_W: f32 = 1.55; // widest a landscape tile may get
const GAP_X: f32 = 0.16;
const GAP_Y: f32 = 0.16;
const CELL_X: f32 = MAX_W + GAP_X; // column pitch (1.71)
const CELL_Y: f32 = TILE + GAP_Y;
const FOV_Y: f32 = 45.0 * std::f32::consts::PI / 180.0; // 45°, like the web camera
const BASE_DIST: f32 = 7.2; // default camera distance (wheel zooms between MIN..MAX)
const MIN_DIST: f32 = 4.5;
const MAX_DIST: f32 = 13.0;
const FOCUS_DIST: f32 = 5.6; // distance when a tile is focused
const CAM_Y: f32 = 0.3; // slight downward camera offset
const BANK_GAIN: f32 = 0.22; // sqrt(|vel|) → bank radians
const BANK_MAX: f32 = 0.5;
const PAN_Y_MAX: f32 = 1.7; // vertical grab-pan limit
const DRAG_GAIN: f32 = 0.6; // left-drag scroll sensitivity
const SCRUB_ZONE_PX: f32 = 44.0; // bottom band that acts as the scrubber
const BTN_X: f32 = 12.0; // Open button (toolbar, top-left), pixels
const BTN_Y: f32 = 12.0;
const BTN_W: f32 = 84.0;
const BTN_H: f32 = 34.0;
const ARROW_W: f32 = 54.0; // lightbox prev/next buttons (vertically centered on each edge)
const ARROW_H: f32 = 84.0;
const ARROW_MARGIN: f32 = 18.0;
const ACCEL: f32 = 22.0; // arrow-key acceleration (world units / s²)
const MAX_SPEED: f32 = 11.0;
const DAMP: f32 = 6.0; // scroll velocity damping
const ANIM_SPEED: f32 = 4.0; // focus in/out transition speed (1 / seconds)

const POOL: u32 = 128; // resident texture-array layers (fixed VRAM ceiling: POOL * 1MB)
const KEEP_COLS: i64 = 16; // columns kept resident on each side of the camera
const MAX_INFLIGHT: usize = 8; // concurrent decodes (throttle, like the JS MAX_INFLIGHT)
const MAX_UPLOADS_PER_FRAME: usize = 4; // GPU texture uploads/frame (spread bursts → smooth scroll)
const WORKERS: usize = 4; // decode threads

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    offset: [f32; 2],
    size: [f32; 2],
    layer: u32,
    uv_extent: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

const QUAD: [Vertex; 4] = [
    Vertex { pos: [0.0, 0.0], uv: [0.0, 1.0] },
    Vertex { pos: [1.0, 0.0], uv: [1.0, 1.0] },
    Vertex { pos: [0.0, 1.0], uv: [0.0, 0.0] },
    Vertex { pos: [1.0, 1.0], uv: [1.0, 0.0] },
];
const INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];

const VERTEX_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![2 => Float32x2, 3 => Float32x2, 4 => Uint32, 5 => Float32x2];

/// A screen-space coloured rectangle (NDC). Used for the bottom scrubber bar overlay.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayRect {
    rect: [f32; 4], // x, y (NDC bottom-left) + w, h (NDC)
    color: [f32; 4],
}
const OVERLAY_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
const OVERLAY_CAP: u64 = 8;
const OVERLAY_SHADER: &str = r#"
struct In { @location(0) rect: vec4<f32>, @location(1) color: vec4<f32> };
struct V { @builtin(position) clip: vec4<f32>, @location(0) color: vec4<f32> };
@vertex
fn vs(@builtin(vertex_index) vi: u32, in: In) -> V {
    var c = array<vec2<f32>, 6>(
        vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let p = in.rect.xy + c[vi] * in.rect.zw;
    var out: V;
    out.clip = vec4(p, 0.0, 1.0);
    out.color = in.color;
    return out;
}
@fragment
fn fs(in: V) -> @location(0) vec4<f32> { return in.color; }
"#;

/// Lightbox: the focused image drawn as a fitted, centered screen-space quad over a dimmed wall.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LbUniform {
    rect: [f32; 4],     // NDC x, y (bottom-left), w, h
    uv_layer: [f32; 4], // uv.x, uv.y (extent), layer, alpha
}
const LIGHTBOX_SHADER: &str = r#"
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var samp: sampler;
struct LB { rect: vec4<f32>, uv_layer: vec4<f32> };
@group(1) @binding(0) var<uniform> lb: LB;
struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    var c = array<vec2<f32>, 6>(
        vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let q = c[i];
    let p = lb.rect.xy + q * lb.rect.zw;
    var out: V;
    out.clip = vec4(p, 0.0, 1.0);
    out.uv = vec2(q.x, 1.0 - q.y) * lb.uv_layer.xy;
    return out;
}
@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    let c = textureSample(atlas, samp, in.uv, i32(lb.uv_layer.z));
    return vec4(c.rgb, lb.uv_layer.w);
}
"#;

/// Where a tile's pixels come from — an image file, a video file (shown as a play tile, played on
/// focus), or a generated placeholder when no folder is given.
#[derive(Clone)]
pub enum Source {
    File(PathBuf),
    Video(PathBuf),
    Placeholder(usize),
}

struct Job {
    index: usize,
    source: Source,
    gen: u64, // library generation — results from an old library are dropped
}
struct Loaded {
    index: usize,
    rgba: Vec<u8>, // empty == decode failed
    w: u32,        // resized dims (≤ TILE_PX), preserving aspect
    h: u32,
    gen: u64,
}

/// Per-resident-tile status.
enum Tile {
    Loading,
    Ready {
        layer: u32,    // assigned texture-array layer
        aspect: f32,   // image w/h → quad width
        uv: [f32; 2],  // fraction of the layer the image fills
    },
    Failed,
}

/// What a held pointer is doing — matches the web wall: left-drag scrolls, right/middle-drag
/// grab-pans, the bottom band scrubs.
#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    None,
    Scroll,
    Pan,
    Scrub,
}

pub struct State {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,

    pipeline: wgpu::RenderPipeline,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    num_instances: u32,
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_buf: wgpu::Buffer,
    lightbox_pipeline: wgpu::RenderPipeline,
    lb_buf: wgpu::Buffer,
    lb_bg: wgpu::BindGroup,
    ui: crate::ui::Ui,

    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    camera_bgl: wgpu::BindGroupLayout,
    tex_bg: wgpu::BindGroup,
    tex: wgpu::Texture,

    // wall library + residency
    sources: Arc<Vec<Source>>,
    total: usize,
    total_cols: i64,
    generation: u64,
    current_folder: Option<PathBuf>,
    resident: HashMap<usize, Tile>,
    free_layers: Vec<u32>,
    inflight: usize,

    // decode worker pool
    job_tx: Sender<Job>,
    result_rx: Receiver<Loaded>,

    // camera / scroll
    scroll_x: f32,
    prev_scroll_x: f32, // last frame's scroll_x — measures real drag speed for banking
    velocity: f32,
    input_dir: f32,
    scroll_max: f32,
    cam_dist: f32,        // current (smoothed) camera distance
    cam_dist_target: f32, // wheel-driven zoom target
    pan_y: f32,           // vertical grab-pan
    drag_mode: DragMode,
    drag_last_x: f32,
    drag_last_y: f32,
    drag_moved: bool,
    open_requested: bool, // the Open button was clicked (main opens the picker)
    scanning: bool,       // a folder is being picked/scanned on a worker thread
    focus: Option<usize>, // currently-focused tile
    focus_t: f32,         // 0 = wall, 1 = focused (animated)
    video: Option<crate::video::Player>, // playing the focused video tile, if any
    video_for: Option<usize>,            // which tile self.video belongs to
    last_frame: Instant,
    frame: u64,
}

impl State {
    pub async fn new(window: Arc<Window>, folder: Option<PathBuf>) -> State {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no suitable GPU adapter found");
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("wall-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await
            .expect("failed to create device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // --- library (paths only — cheap, even for 16k) ---
        let sources = Arc::new(gather_sources(folder.clone()));
        let total = sources.len();
        let total_cols = (total.div_ceil(ROWS)) as i64;
        let scroll_max = (total_cols - 1).max(0) as f32 * CELL_X;
        log::info!("library: {total} tiles ({total_cols} columns)");

        // --- decode worker pool ---
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<Loaded>();
        for _ in 0..WORKERS {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            std::thread::spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let (rgba, w, h) = decode(&job.source);
                    if result_tx
                        .send(Loaded {
                            index: job.index,
                            rgba,
                            w,
                            h,
                            gen: job.gen,
                        })
                        .is_err()
                    {
                        break; // main gone
                    }
                }
            });
        }

        // --- texture-array pool (fixed VRAM) ---
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile-pool"),
            size: wgpu::Extent3d {
                width: TILE_PX,
                height: TILE_PX,
                depth_or_array_layers: POOL,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let tex_view = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let free_layers: Vec<u32> = (0..POOL).rev().collect();

        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
        let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tex-bg"),
            layout: &tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // --- camera uniform ---
        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-bgl"),
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
        let camera_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        // --- geometry + dynamic instance buffer (POOL capacity) ---
        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-vb"),
            contents: bytemuck::cast_slice(&QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-ib"),
            contents: bytemuck::cast_slice(&INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: POOL as u64 * size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- pipeline ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tile-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pl_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl"),
            bind_group_layouts: &[&camera_bgl, &tex_bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tile-pipeline"),
            layout: Some(&pl_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &VERTEX_ATTRS,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE_ATTRS,
                    },
                ],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // --- overlay (2D screen-space rects: the scrubber bar) ---
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-shader"),
            source: wgpu::ShaderSource::Wgsl(OVERLAY_SHADER.into()),
        });
        let overlay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay-pl"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&overlay_layout),
            vertex: wgpu::VertexState {
                module: &overlay_shader,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<OverlayRect>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &OVERLAY_ATTRS,
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &overlay_shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let overlay_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay"),
            size: OVERLAY_CAP * size_of::<OverlayRect>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ui = crate::ui::Ui::new(&device, &queue, config.format);

        // --- lightbox (focused image fitted over the dimmed wall; reuses the tile texture array) ---
        let lb_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lightbox"),
            size: size_of::<LbUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lb_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lb-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let lb_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lb-bg"),
            layout: &lb_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: lb_buf.as_entire_binding(),
            }],
        });
        let lb_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lightbox-shader"),
            source: wgpu::ShaderSource::Wgsl(LIGHTBOX_SHADER.into()),
        });
        let lb_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lb-pl"),
            bind_group_layouts: &[&tex_bgl, &lb_bgl],
            push_constant_ranges: &[],
        });
        let lightbox_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lightbox-pipeline"),
            layout: Some(&lb_layout),
            vertex: wgpu::VertexState {
                module: &lb_shader,
                entry_point: "vs",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &lb_shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let mut state = State {
            window,
            surface,
            device,
            queue,
            config,
            size,
            pipeline,
            vertex_buf,
            index_buf,
            instance_buf,
            num_instances: 0,
            overlay_pipeline,
            overlay_buf,
            lightbox_pipeline,
            lb_buf,
            lb_bg,
            ui,
            camera_buf,
            camera_bg,
            camera_bgl,
            tex_bg,
            tex,
            sources,
            total,
            total_cols,
            generation: 0,
            current_folder: folder,
            resident: HashMap::new(),
            free_layers,
            inflight: 0,
            job_tx,
            result_rx,
            scroll_x: 0.0,
            prev_scroll_x: 0.0,
            velocity: 0.0,
            input_dir: 0.0,
            scroll_max,
            cam_dist: BASE_DIST,
            cam_dist_target: BASE_DIST,
            pan_y: 0.0,
            drag_mode: DragMode::None,
            drag_last_x: 0.0,
            drag_last_y: 0.0,
            drag_moved: false,
            open_requested: false,
            scanning: false,
            focus: None,
            focus_t: 0.0,
            video: None,
            video_for: None,
            last_frame: Instant::now(),
            frame: 0,
        };
        state.upload_camera();
        // Test hook: auto-focus a tile on startup (e.g. COOLIRIS_FOCUS=0 to play a video tile).
        if let Some(i) = std::env::var("COOLIRIS_FOCUS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        {
            if i < state.total {
                state.focus = Some(i);
            }
        }
        state
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.size = size;
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
            self.upload_camera();
        }
    }

    /// Mouse wheel: vertical = zoom (camera distance), horizontal (trackpad) = pan. Web feel.
    pub fn wheel(&mut self, dx: f32, dy: f32) {
        if self.focus.is_some() {
            return;
        }
        if dx.abs() > dy.abs() {
            self.velocity += dx * 0.03;
        } else {
            self.cam_dist_target = (self.cam_dist_target + dy * 0.01).clamp(MIN_DIST, MAX_DIST);
        }
    }

    /// Pointer pressed (button: 0 left, 1 middle, 2 right). Bottom band scrubs; middle/right
    /// grab-pan; left drags to scroll (or clicks to select).
    pub fn pointer_down(&mut self, button: u8, x: f32, y: f32) {
        self.drag_last_x = x;
        self.drag_last_y = y;
        self.drag_moved = false;
        // The Open button (top-left toolbar).
        if button == 0 && x >= BTN_X && x <= BTN_X + BTN_W && y >= BTN_Y && y <= BTN_Y + BTN_H {
            self.open_requested = true;
            self.drag_mode = DragMode::None;
            return;
        }
        // Lightbox prev/next arrow buttons (only while an item is open).
        if button == 0 && self.focus.is_some() {
            let (prev, next) = self.arrow_rects();
            if hit(prev, x, y) {
                self.navigate(-1);
                self.drag_mode = DragMode::None;
                return;
            }
            if hit(next, x, y) {
                self.navigate(1);
                self.drag_mode = DragMode::None;
                return;
            }
        }
        let h = self.config.height as f32;
        if self.focus.is_none() && self.scroll_max > 0.0 && y > h - SCRUB_ZONE_PX {
            self.drag_mode = DragMode::Scrub;
            self.scrub_to(x);
        } else if button == 1 || button == 2 {
            self.drag_mode = DragMode::Pan;
        } else {
            self.drag_mode = DragMode::Scroll;
            self.velocity = 0.0;
        }
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        if self.drag_mode == DragMode::None {
            return;
        }
        let dx = x - self.drag_last_x;
        let dy = y - self.drag_last_y;
        self.drag_last_x = x;
        self.drag_last_y = y;
        let h = self.config.height.max(1) as f32;
        let max = self.scroll_max.max(0.0);
        match self.drag_mode {
            DragMode::Scrub => self.scrub_to(x),
            DragMode::Pan => {
                let vph = self.viewport_h();
                self.pan_y = (self.pan_y + dy / h * vph).clamp(-PAN_Y_MAX, PAN_Y_MAX);
                if self.focus.is_none() {
                    self.scroll_x = (self.scroll_x - dx / h * vph).clamp(0.0, max);
                }
            }
            DragMode::Scroll => {
                if dx.abs() > 2.0 {
                    self.drag_moved = true;
                }
                if self.focus.is_none() {
                    let vpw = self.viewport_h() * (self.config.width.max(1) as f32 / h);
                    let world = dx / h * vpw * DRAG_GAIN;
                    self.scroll_x = (self.scroll_x - world).clamp(0.0, max);
                    // velocity (for bank + release fling) is measured from real motion in update().
                }
            }
            DragMode::None => {}
        }
    }

    /// Pointer released: a left press with no drag is a click → select / deselect.
    pub fn pointer_up(&mut self, _button: u8) {
        let mode = self.drag_mode;
        self.drag_mode = DragMode::None;
        if mode == DragMode::Scroll && !self.drag_moved {
            if self.focus.is_some() {
                self.recenter_on_focus(); // leave the wall on the photo you were viewing
                self.focus = None;
            } else if let Some(i) = self.pick(self.drag_last_x, self.drag_last_y) {
                self.focus = Some(i);
            }
        }
    }

    /// Scrubber track geometry in pixels: (left pad, track width, thumb width). The thumb width
    /// reflects how much of the library is on screen, like a real scrollbar.
    fn scrubber_geom(&self) -> (f32, f32, f32) {
        let w = self.config.width.max(1) as f32;
        let h = self.config.height.max(1) as f32;
        let pad = 16.0;
        let track_w = (w - 2.0 * pad).max(1.0);
        let vpw = self.viewport_h() * (w / h);
        let content = self.total_cols.max(1) as f32 * CELL_X;
        let thumb_w = track_w * (vpw / content).clamp(0.06, 1.0);
        (pad, track_w, thumb_w)
    }

    fn scrub_to(&mut self, x: f32) {
        let (pad, track_w, thumb_w) = self.scrubber_geom();
        let travel = (track_w - thumb_w).max(1.0);
        // Center the thumb under the cursor so it tracks the pointer 1:1 (a real scrollbar grab).
        let frac = ((x - pad - thumb_w * 0.5) / travel).clamp(0.0, 1.0);
        self.scroll_x = frac * self.scroll_max.max(0.0);
        self.velocity = 0.0;
    }

    fn viewport_h(&self) -> f32 {
        2.0 * (FOV_Y * 0.5).tan() * self.cam_dist
    }

    /// Lightbox prev/next button rects (x, y, w, h, in pixels): (prev on the left, next on the
    /// right). Shared by hit-testing, the overlay backgrounds and the glyph placement.
    fn arrow_rects(&self) -> ([f32; 4], [f32; 4]) {
        let w = self.config.width as f32;
        let h = self.config.height as f32;
        let y = (h - ARROW_H) * 0.5;
        let prev = [ARROW_MARGIN, y, ARROW_W, ARROW_H];
        let next = [w - ARROW_MARGIN - ARROW_W, y, ARROW_W, ARROW_H];
        (prev, next)
    }

    /// Arrow keys: -1 left, +1 right, 0 released.
    pub fn set_dir(&mut self, dir: f32) {
        self.input_dir = dir;
    }

    pub fn update(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;

        // --- scroll physics (frozen while a tile is focused) ---
        // A live pointer drag owns scroll_x directly (set in pointer_move / scrub_to). Integrating
        // inertia on top would double-move it — and, worse, keep it drifting and banked while the
        // cursor holds still. So while dragging we only *measure* the actual per-frame speed (low-
        // passed) to drive the bank; that measured speed then becomes the fling when you let go.
        if self.focus.is_none() {
            let max = self.scroll_max.max(0.0);
            match self.drag_mode {
                DragMode::Scroll => {
                    let measured = (self.scroll_x - self.prev_scroll_x) / dt.max(1e-4);
                    self.velocity += (measured - self.velocity) * 0.35; // low-pass → stable bank
                    self.velocity = self.velocity.clamp(-MAX_SPEED, MAX_SPEED);
                }
                DragMode::Scrub | DragMode::Pan => self.velocity = 0.0, // direct control, no bank
                DragMode::None => {
                    if self.input_dir != 0.0 {
                        self.velocity = (self.velocity + self.input_dir * ACCEL * dt)
                            .clamp(-MAX_SPEED, MAX_SPEED);
                    } else {
                        self.velocity *= (1.0 - DAMP * dt).max(0.0);
                        if self.velocity.abs() < 0.001 {
                            self.velocity = 0.0;
                        }
                    }
                    self.scroll_x = (self.scroll_x + self.velocity * dt).clamp(0.0, max);
                    if self.scroll_x <= 0.0 || self.scroll_x >= max {
                        self.velocity = 0.0;
                    }
                }
            }
        } else {
            self.velocity = 0.0;
        }
        self.prev_scroll_x = self.scroll_x;

        // --- focus in/out transition ---
        let target_t = if self.focus.is_some() { 1.0 } else { 0.0 };
        let step = ANIM_SPEED * dt;
        self.focus_t = if self.focus_t < target_t {
            (self.focus_t + step).min(target_t)
        } else {
            (self.focus_t - step).max(target_t)
        };

        // --- camera zoom smoothing (wheel target, or pull-in when focused) ---
        let target_dist = if self.focus.is_some() {
            FOCUS_DIST
        } else {
            self.cam_dist_target
        };
        self.cam_dist += (target_dist - self.cam_dist) * (6.0 * dt).min(1.0);

        // --- drain finished decodes: assign a layer + upload, or drop if no longer wanted ---
        // Cap GPU uploads per frame: a fast scroll can finish many decodes at once, and uploading
        // them all in one frame hitches. Spread them over frames — the rest stay queued.
        let mut uploads = 0;
        while uploads < MAX_UPLOADS_PER_FRAME {
            let Ok(res) = self.result_rx.try_recv() else {
                break;
            };
            self.inflight = self.inflight.saturating_sub(1);
            if res.gen != self.generation {
                continue; // result from a previous library (folder was swapped)
            }
            if !matches!(self.resident.get(&res.index), Some(Tile::Loading)) {
                continue; // evicted while in flight
            }
            // Keep the focused item even if it scrolled out of the window (prev/next can move it).
            let wanted = self.in_window(res.index) || Some(res.index) == self.focus;
            if res.rgba.is_empty() || res.w == 0 || res.h == 0 {
                self.resident.insert(res.index, Tile::Failed);
            } else if wanted {
                if let Some(layer) = self.free_layers.pop() {
                    self.upload_layer(layer, &res.rgba, res.w, res.h);
                    self.resident.insert(
                        res.index,
                        Tile::Ready {
                            layer,
                            aspect: res.w as f32 / res.h as f32,
                            uv: [
                                res.w as f32 / TILE_PX as f32,
                                res.h as f32 / TILE_PX as f32,
                            ],
                        },
                    );
                    uploads += 1;
                } else {
                    self.resident.remove(&res.index); // pool full (shouldn't happen) — retry later
                }
            } else {
                self.resident.remove(&res.index); // scrolled away mid-decode
            }
        }

        // --- evict resident tiles outside the window (free their layers) ---
        let (first, last) = self.window_cols();
        let evict: Vec<usize> = self
            .resident
            .iter()
            .filter(|(i, t)| {
                let col = (**i / ROWS) as i64;
                (col < first || col > last)
                    && !matches!(t, Tile::Loading)
                    && Some(**i) != self.focus // never evict the open (focused) item
            })
            .map(|(i, _)| *i)
            .collect();
        for i in evict {
            if let Some(Tile::Ready { layer, .. }) = self.resident.remove(&i) {
                self.free_layers.push(layer);
            }
        }

        // Always keep the open (focused) item loaded — prev/next can move it outside the window.
        // Dispatch it ahead of everything else and ignore the inflight cap, so navigating to a
        // not-yet-loaded image starts decoding it this frame instead of showing "Loading…" while
        // it waits behind window tiles.
        if let Some(f) = self.focus {
            if f < self.total && !self.resident.contains_key(&f) {
                self.resident.insert(f, Tile::Loading);
                self.inflight += 1;
                let _ = self.job_tx.send(Job {
                    index: f,
                    source: self.sources[f].clone(),
                    gen: self.generation,
                });
            }
        }

        // --- dispatch new loads, nearest column first, throttled ---
        let center = self.view_center();
        'outer: for d in 0..=KEEP_COLS {
            for side in 0..2 {
                if d == 0 && side == 1 {
                    break;
                }
                let col = if side == 0 { center - d } else { center + d };
                if col < first || col > last {
                    continue;
                }
                for row in 0..ROWS {
                    if self.inflight >= MAX_INFLIGHT {
                        break 'outer;
                    }
                    let idx = col as usize * ROWS + row;
                    if idx >= self.total || self.resident.contains_key(&idx) {
                        continue;
                    }
                    self.resident.insert(idx, Tile::Loading);
                    self.inflight += 1;
                    let _ = self.job_tx.send(Job {
                        index: idx,
                        source: self.sources[idx].clone(),
                        gen: self.generation,
                    });
                }
            }
        }

        self.rebuild_instances();
        self.upload_camera();

        // --- focused-video playback: start mpv when a video tile is focused, stop on change ---
        if self.focus != self.video_for {
            self.video = None; // dropping the player stops mpv
            self.video_for = self.focus;
            if let Some(idx) = self.focus {
                if let Source::Video(path) = self.sources[idx].clone() {
                    self.video = Some(crate::video::Player::start(
                        &self.device,
                        &self.queue,
                        &self.camera_bgl,
                        self.config.format,
                        &path,
                    ));
                }
            }
        }
        if let Some(v) = &mut self.video {
            v.update(&self.device, &self.queue);
        }

        // Opt-in streaming readout: RUST_LOG=cooliris_rs=debug
        self.frame = self.frame.wrapping_add(1);
        if log::log_enabled!(log::Level::Debug) && self.frame % 120 == 0 {
            log::debug!(
                "scroll {:.1} | resident {} (drawn {}) | inflight {} | free layers {}/{}",
                self.scroll_x,
                self.resident.len(),
                self.num_instances,
                self.inflight,
                self.free_layers.len(),
                POOL,
            );
        }
    }

    /// Column the streaming window centers on: the focused item while the lightbox is open (so its
    /// neighbours decode in the background → prev/next is instant), else the scrolled position.
    fn view_center(&self) -> i64 {
        match self.focus {
            Some(f) => (f / ROWS) as i64,
            None => (self.scroll_x / CELL_X).round() as i64,
        }
    }

    fn window_cols(&self) -> (i64, i64) {
        let center = self.view_center();
        (
            (center - KEEP_COLS).max(0),
            (center + KEEP_COLS).min(self.total_cols - 1),
        )
    }

    fn in_window(&self, idx: usize) -> bool {
        let (f, l) = self.window_cols();
        let col = (idx / ROWS) as i64;
        col >= f && col <= l
    }

    /// Upload the image into the top-left w×h sub-region of its layer (the rest is unused and
    /// never sampled, thanks to uv_extent).
    fn upload_layer(&self, layer: u32, rgba: &[u8], w: u32, h: u32) {
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    fn rebuild_instances(&mut self) {
        let mut inst: Vec<Instance> = Vec::with_capacity(self.resident.len());
        for (&i, t) in &self.resident {
            if let Tile::Ready { layer, aspect, uv } = *t {
                // Fixed row height; width follows the image aspect, capped so tiles stay in their
                // column. Centered in the cell.
                let mut w = TILE * aspect;
                let mut h = TILE;
                if w > MAX_W {
                    w = MAX_W;
                    h = MAX_W / aspect;
                }
                let col = (i / ROWS) as f32;
                let row = (i % ROWS) as f32;
                inst.push(Instance {
                    offset: [col * CELL_X, (row - 1.0) * CELL_Y],
                    size: [w, h],
                    layer,
                    uv_extent: uv,
                });
            }
        }
        self.num_instances = inst.len() as u32;
        if !inst.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&inst));
        }
    }

    fn tile_center(&self, i: usize) -> (f32, f32) {
        let col = (i / ROWS) as f32;
        let row = (i % ROWS) as f32;
        (col * CELL_X, (row - 1.0) * CELL_Y)
    }

    /// Eye/target for the wall, blended toward the focused tile by `s` (0..1).
    fn eye_target(&self, s: f32) -> (f32, f32) {
        let (fx, fy) = self
            .focus
            .map(|i| self.tile_center(i))
            .unwrap_or((self.scroll_x, self.pan_y));
        let x = self.scroll_x + (fx - self.scroll_x) * s;
        let y = CAM_Y + self.pan_y + (fy - self.pan_y) * s;
        (x, y)
    }

    fn upload_camera(&self) {
        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let proj = Mat4::perspective_rh(FOV_Y, aspect, 0.1, 100.0);
        let s = {
            let t = self.focus_t.clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let (ex, ey) = self.eye_target(s);
        let eye = Vec3::new(ex, ey, self.cam_dist);
        let tgt = Vec3::new(ex, ey, 0.0);
        let view = Mat4::look_at_rh(eye, tgt, Vec3::Y);

        // Bank: sqrt(|velocity|) swing as you scroll (banks on slow scroll too), faded out on focus.
        let v = self.velocity;
        let bank =
            (v.signum() * v.abs().sqrt() * BANK_GAIN).clamp(-BANK_MAX, BANK_MAX) * (1.0 - s);
        let pivot = Vec3::new(ex, 0.0, 0.0);
        let model = Mat4::from_translation(pivot)
            * Mat4::from_rotation_y(bank)
            * Mat4::from_translation(-pivot);

        let u = CameraUniform {
            view_proj: (proj * view * model).to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::cast_slice(&[u]));
    }

    /// Ray-pick the tile under a screen-space click (against the z=0 wall plane).
    fn pick(&self, cx: f32, cy: f32) -> Option<usize> {
        let w = self.config.width as f32;
        let h = self.config.height.max(1) as f32;
        let aspect = w / h;
        let (ex, ey) = self.eye_target(0.0); // wall view (we only pick when not focused)
        let eye = Vec3::new(ex, ey, self.cam_dist);
        let view = Mat4::look_at_rh(eye, Vec3::new(ex, ey, 0.0), Vec3::Y);
        let proj = Mat4::perspective_rh(FOV_Y, aspect, 0.1, 100.0);
        let inv = (proj * view).inverse();

        let ndc_x = 2.0 * cx / w - 1.0;
        let ndc_y = 1.0 - 2.0 * cy / h;
        let near = inv * Vec4::new(ndc_x, ndc_y, 0.0, 1.0); // wgpu NDC near z = 0
        let far = inv * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let near = near.truncate() / near.w;
        let far = far.truncate() / far.w;
        let dir = far - near;
        if dir.z.abs() < 1e-6 {
            return None;
        }
        let t = -near.z / dir.z;
        if t < 0.0 {
            return None;
        }
        let hit = near + dir * t; // world point on the wall plane

        let col = (hit.x / CELL_X).round();
        let row = (hit.y / CELL_Y).round() + 1.0;
        if col < 0.0 || row < 0.0 || row >= ROWS as f32 {
            return None;
        }
        let idx = col as usize * ROWS + row as usize;
        (idx < self.total).then_some(idx)
    }

    /// Esc / back: return to the wall, landing on the photo you were viewing (after prev/next).
    pub fn back(&mut self) {
        self.recenter_on_focus();
        self.focus = None;
    }

    /// Move the wall under the focused item so closing the lightbox leaves you where you stopped,
    /// not back where you opened from. Invisible while focused (the camera is locked to the tile),
    /// so the only effect is where the zoom-out lands.
    fn recenter_on_focus(&mut self) {
        if let Some(f) = self.focus {
            let col = (f / ROWS) as f32;
            self.scroll_x = (col * CELL_X).clamp(0.0, self.scroll_max.max(0.0));
            self.prev_scroll_x = self.scroll_x;
            self.velocity = 0.0;
        }
    }

    pub fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Prev/next item in the lightbox (dir = -1 / +1).
    pub fn navigate(&mut self, dir: i64) {
        if let (Some(f), true) = (self.focus, self.total > 0) {
            self.focus = Some((f as i64 + dir).clamp(0, self.total as i64 - 1) as usize);
        }
    }

    /// True while the open item's image hasn't finished decoding yet.
    fn focus_loading(&self) -> bool {
        match self.focus {
            Some(i) => !matches!(self.resident.get(&i), Some(Tile::Ready { .. })),
            None => false,
        }
    }

    pub fn current_folder(&self) -> Option<&std::path::Path> {
        self.current_folder.as_deref()
    }

    /// Swap the library to a pre-scanned set of sources (scanning happens off the main thread so
    /// a big/slow folder doesn't freeze the window). The generation bump drops in-flight decodes
    /// from the old library; the texture pool, pipelines and worker threads are all reused.
    pub fn reload_with(&mut self, folder: Option<PathBuf>, sources: Vec<Source>) {
        self.generation += 1;
        self.sources = Arc::new(sources);
        self.current_folder = folder;
        self.total = self.sources.len();
        self.total_cols = self.total.div_ceil(ROWS) as i64;
        self.scroll_max = (self.total_cols - 1).max(0) as f32 * CELL_X;
        self.resident.clear();
        self.free_layers = (0..POOL).rev().collect();
        self.inflight = 0;
        self.scroll_x = 0.0;
        self.velocity = 0.0;
        self.pan_y = 0.0;
        self.cam_dist_target = BASE_DIST;
        self.focus = None;
        self.video = None;
        self.video_for = None;
        self.num_instances = 0;
        self.scanning = false;
        log::info!("loaded {} tiles", self.total);
    }

    /// Whether the Open button was clicked since the last check (main opens the picker).
    pub fn take_open_request(&mut self) -> bool {
        std::mem::take(&mut self.open_requested)
    }

    /// Mark that a folder is being picked/scanned (shows a "Scanning folder…" indicator).
    pub fn set_scanning(&mut self, b: bool) {
        self.scanning = b;
    }

    /// Toolbar text: the Open label + a folder hint or the loaded/total readout.
    fn ui_lines(&self) -> Vec<crate::ui::Line> {
        let mut v = vec![crate::ui::Line {
            text: "Open".into(),
            x: BTN_X + 14.0,
            y: BTN_Y + 8.0,
            size: 17.0,
            color: [235, 235, 240, 255],
        }];
        let status = if self.scanning {
            "scanning folder…".to_string()
        } else if self.current_folder.is_none() {
            "drop a folder here · click Open · press O".to_string()
        } else if self.inflight > 0 {
            format!("{} items · loading…", self.total)
        } else {
            format!("{} items", self.total)
        };
        v.push(crate::ui::Line {
            text: status,
            x: BTN_X + BTN_W + 16.0,
            y: BTN_Y + 9.0,
            size: 15.0,
            color: [205, 205, 215, 235],
        });

        let cx = self.config.width as f32 * 0.5;
        let cy = self.config.height as f32 * 0.5;
        if self.scanning {
            v.push(crate::ui::Line {
                text: "Scanning folder…".into(),
                x: cx - 92.0,
                y: cy - 20.0,
                size: 30.0,
                color: [235, 235, 240, 255],
            });
        } else if let Some(idx) = self.focus {
            // Lightbox: clickable prev/next chevrons (drawn only when a neighbour exists), the item
            // position + hint, and "Loading…" until the image decodes.
            let (prev, next) = self.arrow_rects();
            if idx > 0 {
                v.push(crate::ui::Line {
                    text: "‹".into(),
                    x: prev[0] + ARROW_W * 0.5 - 9.0,
                    y: prev[1] + ARROW_H * 0.5 - 30.0,
                    size: 46.0,
                    color: [240, 240, 245, 240],
                });
            }
            if idx + 1 < self.total {
                v.push(crate::ui::Line {
                    text: "›".into(),
                    x: next[0] + ARROW_W * 0.5 - 9.0,
                    y: next[1] + ARROW_H * 0.5 - 30.0,
                    size: 46.0,
                    color: [240, 240, 245, 240],
                });
            }
            v.push(crate::ui::Line {
                text: format!("{} / {}   ·   click ‹ › or ← →   ·   Esc", idx + 1, self.total),
                x: cx - 140.0,
                y: self.config.height as f32 - 40.0,
                size: 16.0,
                color: [225, 225, 230, 235],
            });
            if self.focus_loading() {
                v.push(crate::ui::Line {
                    text: "Loading…".into(),
                    x: cx - 52.0,
                    y: cy - 20.0,
                    size: 30.0,
                    color: [235, 235, 240, 255],
                });
            }
        } else {
            // Big centered "Loading…" right after opening a folder, while the first tiles decode.
            let ready = self
                .resident
                .values()
                .filter(|t| matches!(t, Tile::Ready { .. }))
                .count();
            if self.inflight > 0 && ready < 6 {
                v.push(crate::ui::Line {
                    text: "Loading…".into(),
                    x: cx - 52.0,
                    y: cy - 20.0,
                    size: 30.0,
                    color: [235, 235, 240, 255],
                });
            }
        }
        v
    }

    /// Fit the focused image to the screen (preserving aspect), fade it in, and write the
    /// lightbox uniform. Returns whether there's an image to draw (false for videos / not loaded).
    fn prepare_lightbox(&self) -> bool {
        let Some(idx) = self.focus else {
            return false;
        };
        if matches!(self.sources.get(idx), Some(Source::Video(_))) {
            return false; // videos play via the video layer, not the lightbox
        }
        let Some(Tile::Ready { layer, aspect, uv }) = self.resident.get(&idx) else {
            return false; // still decoding
        };
        let w = self.config.width.max(1) as f32;
        let h = self.config.height.max(1) as f32;
        let screen_aspect = w / h;
        let margin = 0.92; // leave a border around the fitted image
        let (qw, qh) = if *aspect > screen_aspect {
            (2.0 * margin, 2.0 * margin * screen_aspect / *aspect)
        } else {
            (2.0 * margin * *aspect / screen_aspect, 2.0 * margin)
        };
        let u = LbUniform {
            rect: [-qw / 2.0, -qh / 2.0, qw, qh],
            uv_layer: [uv[0], uv[1], *layer as f32, smoothstep(self.focus_t)],
        };
        self.queue
            .write_buffer(&self.lb_buf, 0, bytemuck::cast_slice(&[u]));
        true
    }

    /// Screen-space overlay rects: the Open button background, a top loading bar, and the bottom
    /// scrubber track + thumb.
    fn overlay_rects(&self) -> Vec<OverlayRect> {
        let w = self.config.width.max(1) as f32;
        let h = self.config.height.max(1) as f32;
        let nx = |px: f32| px / w * 2.0 - 1.0;
        let nw = |px: f32| px / w * 2.0;
        let ny_top = |px: f32| 1.0 - px / h * 2.0; // px from top → NDC y
        let nhh = |px: f32| px / h * 2.0;
        let mut rects = Vec::new();

        // [0] Lightbox dim — full-screen, ramps with focus (alpha 0 when not focused). Drawn
        // before the fitted image; the rest of the overlay is drawn after it.
        let s = smoothstep(self.focus_t);
        rects.push(OverlayRect {
            rect: [-1.0, -1.0, 2.0, 2.0],
            color: [0.02, 0.02, 0.03, 0.93 * s],
        });

        // Open button background.
        rects.push(OverlayRect {
            rect: [nx(BTN_X), ny_top(BTN_Y + BTN_H), nw(BTN_W), nhh(BTN_H)],
            color: [1.0, 1.0, 1.0, 0.12],
        });

        // Lightbox prev/next button backgrounds — fade in with the focus, hidden at the ends.
        if let Some(f) = self.focus {
            let to_ndc = |r: [f32; 4]| [nx(r[0]), ny_top(r[1] + r[3]), nw(r[2]), nhh(r[3])];
            let (prev, next) = self.arrow_rects();
            if f > 0 {
                rects.push(OverlayRect {
                    rect: to_ndc(prev),
                    color: [1.0, 1.0, 1.0, 0.14 * s],
                });
            }
            if f + 1 < self.total {
                rects.push(OverlayRect {
                    rect: to_ndc(next),
                    color: [1.0, 1.0, 1.0, 0.14 * s],
                });
            }
        }

        // Top loading bar — shown while tiles are actively decoding; grows as the visible window
        // fills in, then disappears. (A virtualized wall never loads the whole library at once.)
        if self.inflight > 0 {
            let ready = self
                .resident
                .values()
                .filter(|t| matches!(t, Tile::Ready { .. }))
                .count();
            let frac = (ready as f32 / self.resident.len().max(1) as f32).max(0.05);
            let ph = nhh(3.0);
            rects.push(OverlayRect {
                rect: [-1.0, 1.0 - ph, 2.0 * frac, ph],
                color: [0.3, 0.6, 1.0, 0.95],
            });
        }

        // Bottom scrubber (only when scrollable and not focused): a track with a draggable thumb
        // whose width shows how much of the library is on screen, like a scrollbar.
        if self.focus.is_none() && self.scroll_max > 0.0 {
            let (pad, track_w, thumb_w) = self.scrubber_geom();
            let bh = nhh(9.0);
            let by = -1.0 + nhh(12.0);
            rects.push(OverlayRect {
                rect: [nx(pad), by, nw(track_w), bh],
                color: [1.0, 1.0, 1.0, 0.15],
            });
            let frac = (self.scroll_x / self.scroll_max).clamp(0.0, 1.0);
            let thumb_x = pad + (track_w - thumb_w) * frac;
            rects.push(OverlayRect {
                rect: [nx(thumb_x), by, nw(thumb_w), bh],
                color: [0.95, 0.96, 1.0, 0.85],
            });
        }
        rects
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());

        let overlay = self.overlay_rects();
        if !overlay.is_empty() {
            self.queue
                .write_buffer(&self.overlay_buf, 0, bytemuck::cast_slice(&overlay));
        }
        let lb_visible = self.prepare_lightbox();
        let lines = self.ui_lines();
        self.ui.prepare(
            &self.device,
            &self.queue,
            self.config.width,
            self.config.height,
            &lines,
        );

        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wall-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.03,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if self.num_instances > 0 {
                rp.set_pipeline(&self.pipeline);
                rp.set_bind_group(0, &self.camera_bg, &[]);
                rp.set_bind_group(1, &self.tex_bg, &[]);
                rp.set_vertex_buffer(0, self.vertex_buf.slice(..));
                rp.set_vertex_buffer(1, self.instance_buf.slice(..));
                rp.set_index_buffer(self.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                rp.draw_indexed(0..INDICES.len() as u32, 0, 0..self.num_instances);
            }
            // Lightbox dim (overlay rect [0]) — between the wall and the fitted image.
            if !overlay.is_empty() {
                rp.set_pipeline(&self.overlay_pipeline);
                rp.set_vertex_buffer(0, self.overlay_buf.slice(..));
                rp.draw(0..6, 0..1);
            }
            // Playing video draws over its (focused) tile, on top of the dim.
            if let (Some(v), Some(idx)) = (&self.video, self.focus) {
                let (cx, cy) = self.tile_center(idx);
                v.draw(&mut rp, &self.camera_bg, [cx, cy], [1.6, 0.9], &self.queue);
            }
            // Lightbox: the focused image fitted + centered over the dimmed wall.
            if lb_visible {
                rp.set_pipeline(&self.lightbox_pipeline);
                rp.set_bind_group(0, &self.tex_bg, &[]);
                rp.set_bind_group(1, &self.lb_bg, &[]);
                rp.draw(0..6, 0..1);
            }
            // Toolbar + scrubber overlay (rects [1..]).
            if overlay.len() > 1 {
                rp.set_pipeline(&self.overlay_pipeline);
                rp.set_vertex_buffer(0, self.overlay_buf.slice(..));
                rp.draw(0..6, 1..overlay.len() as u32);
            }
            self.ui.render(&mut rp);
        }
        self.queue.submit(std::iter::once(enc.finish()));
        frame.present();
        Ok(())
    }
}

/// Decode + downscale one tile (runs on a worker thread). Resizes to fit TILE_PX preserving
/// aspect; returns (rgba, w, h). Empty/zero == failure.
fn decode(source: &Source) -> (Vec<u8>, u32, u32) {
    match source {
        Source::File(p) => {
            // Decode by CONTENT, not extension — many files are mislabeled (a ".jpg" whose bytes
            // are actually PNG, etc.). with_guessed_format() sniffs the magic bytes.
            let decoded = image::ImageReader::open(p)
                .and_then(|r| r.with_guessed_format())
                .map_err(|e| e.to_string())
                .and_then(|r| r.decode().map_err(|e| e.to_string()));
            match decoded {
                Ok(img) => {
                    let t = img
                        .resize(TILE_PX, TILE_PX, image::imageops::FilterType::Triangle)
                        .to_rgba8();
                    let (w, h) = (t.width(), t.height());
                    (t.into_raw(), w, h)
                }
                Err(e) => {
                    log::debug!("skip {p:?}: {e}");
                    (Vec::new(), 0, 0)
                }
            }
        }
        Source::Video(_) => (video_placeholder(), TILE_PX, TILE_PX),
        Source::Placeholder(i) => (placeholder(*i), TILE_PX, TILE_PX),
    }
}

/// A dark tile with a play triangle — the grid thumbnail for a video (it plays on focus).
fn video_placeholder() -> Vec<u8> {
    let mut buf = vec![0u8; (TILE_PX * TILE_PX * 4) as usize];
    for y in 0..TILE_PX {
        let fy = y as f32 / TILE_PX as f32;
        for x in 0..TILE_PX {
            let i = ((y * TILE_PX + x) * 4) as usize;
            buf[i] = 20;
            buf[i + 1] = 22;
            buf[i + 2] = 28;
            buf[i + 3] = 255;
            let fx = x as f32 / TILE_PX as f32;
            if fx > 0.40 && fx < 0.60 && (fy - 0.5).abs() < (0.60 - fx) * 0.9 {
                buf[i] = 235;
                buf[i + 1] = 235;
                buf[i + 2] = 240;
            }
        }
    }
    buf
}

/// Build the tile library from a folder, scanning subfolders too. Placeholders if none.
pub fn gather_sources(folder: Option<PathBuf>) -> Vec<Source> {
    let Some(dir) = folder else {
        log::info!("no folder chosen — showing placeholders");
        return (0..24).map(Source::Placeholder).collect();
    };
    // Recurse into subfolders to any depth — media is usually nested (a folder per product/album,
    // and those may nest further). follow_links(false) means no symlink loops; the take() caps it.
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(&dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| classify(p).is_some())
        .take(200_000)
        .collect();
    paths.sort();
    log::info!("folder {dir:?}: {} media files (incl. subfolders)", paths.len());
    if paths.is_empty() {
        log::info!("no images/videos under {dir:?} — showing placeholders");
        return (0..24).map(Source::Placeholder).collect();
    }
    paths
        .into_iter()
        .map(|p| match classify(&p) {
            Some(true) => Source::Video(p),
            _ => Source::File(p),
        })
        .collect()
}

/// `Some(true)` = video, `Some(false)` = image, `None` = ignore.
fn classify(p: &std::path::Path) -> Option<bool> {
    match p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v") => Some(true),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp") => Some(false),
        _ => None,
    }
}

/// A simple gradient keyed by index, so the wall is visible without any images.
fn placeholder(i: usize) -> Vec<u8> {
    let hue = (i as f32 * 0.61803398875).fract();
    let (r, g, b) = hsv(hue, 0.5, 0.85);
    let mut buf = vec![0u8; (TILE_PX * TILE_PX * 4) as usize];
    for y in 0..TILE_PX {
        let t = y as f32 / TILE_PX as f32;
        for x in 0..TILE_PX {
            let idx = ((y * TILE_PX + x) * 4) as usize;
            buf[idx] = (r as f32 * (0.4 + 0.6 * t)) as u8;
            buf[idx + 1] = (g as f32 * (0.4 + 0.6 * t)) as u8;
            buf[idx + 2] = (b as f32 * (0.4 + 0.6 * t)) as u8;
            buf[idx + 3] = 255;
        }
    }
    buf
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Point-in-rect test for a pixel-space (x, y, w, h) rect.
fn hit(rect: [f32; 4], x: f32, y: f32) -> bool {
    x >= rect[0] && x <= rect[0] + rect[2] && y >= rect[1] && y <= rect[1] + rect[3]
}

fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32) % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}
