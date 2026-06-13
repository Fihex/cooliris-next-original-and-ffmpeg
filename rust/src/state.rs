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
const FULL_PX: u32 = 2048; // full-res size decoded for the focused photo (crisp lightbox)
const ROWS: usize = 3;
const TILE: f32 = 1.0; // row height (ROW_H)
const MAX_W: f32 = 1.55; // widest a landscape tile may get
const GAP_X: f32 = 0.16;
const GAP_Y: f32 = 0.16;
const CELL_X: f32 = MAX_W + GAP_X; // column pitch (1.71)
const CELL_Y: f32 = TILE + GAP_Y;
const DEFAULT_ASPECT: f32 = 1.4; // assumed aspect before a tile's image has decoded
const REFLECT_GAP: f32 = 0.09; // gap between a photo and its mirrored reflection
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
const VCTL_H: f32 = 60.0; // video controls bar height (pixels, bottom of the screen)
const ACCEL: f32 = 22.0; // arrow-key acceleration (world units / s²)
const MAX_SPEED: f32 = 11.0;
const ANIM_SPEED: f32 = 4.0; // focus in/out transition speed (1 / seconds)

const POOL: u32 = 128; // resident texture-array layers (fixed VRAM ceiling: POOL * 1MB)
const INSTANCE_CAP: u64 = POOL as u64 * 2; // photos + their reflections (bottom row adds ~POOL/3)
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
    kind: u32, // 0 = photo, 1 = mirrored reflection (flips V + fades out)
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
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    2 => Float32x2, 3 => Float32x2, 4 => Uint32, 5 => Float32x2, 6 => Uint32];

/// A screen-space coloured rectangle (NDC). Used for the bottom scrubber bar overlay.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayRect {
    rect: [f32; 4], // x, y (NDC bottom-left) + w, h (NDC)
    color: [f32; 4],
}
const OVERLAY_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
const OVERLAY_CAP: u64 = 64; // dim, toolbar, arrows, scrubber track/thumb + tick lines
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
    gen: u64,   // library generation — results from an old library are dropped
    full: bool, // decode at FULL_PX for the focused lightbox (vs TILE_PX thumbnail)
}
struct Loaded {
    index: usize,
    rgba: Vec<u8>, // empty == decode failed
    w: u32,        // resized dims (≤ TILE_PX or FULL_PX), preserving aspect
    h: u32,
    gen: u64,
    full: bool,
}

/// Pixel rects for the video controls bar (so hit-testing and drawing agree).
struct VideoUi {
    bar: [f32; 4],
    play: [f32; 4],
    seek: [f32; 4], // the track (x, y, w, h)
    audio: [f32; 4],
    subs: [f32; 4],
    full: [f32; 4],
}

/// Which texture the lightbox pass should bind for the focused image.
enum LbDraw {
    None,
    Thumb, // the streamed 512px thumbnail (tile array)
    Full,  // the full-resolution texture
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
    Seek, // dragging the video seek bar
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
    // Full-resolution texture for the focused photo (a single high-res layer the lightbox samples
    // instead of the 512px thumb — bound as an alternate group(0), reusing the lightbox pipeline).
    full_tex: wgpu::Texture,
    full_bg: wgpu::BindGroup,
    full_for: Option<usize>,     // index currently uploaded into full_tex
    full_pending: Option<usize>, // index whose full decode is in flight
    full_extent: [f32; 2],       // fraction of full_tex the image fills
    post: crate::post::Post,     // offscreen scene + blur for the lightbox backdrop
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
    bank: f32, // eased wall lean (lags velocity so it flattens slowly, like the web)
    input_dir: f32,
    scroll_max: f32,
    cam_dist: f32,        // current (smoothed) camera distance
    cam_dist_target: f32, // wheel-driven zoom target
    pan_y: f32,           // vertical grab-pan
    pointer_ndc: [f32; 2], // last cursor position in NDC (zoom centers here)
    last_vp: [f32; 2],     // previous frame's world viewport (w, h) — for zoom-toward-cursor
    drag_mode: DragMode,
    drag_last_x: f32,
    drag_last_y: f32,
    drag_moved: bool,
    open_requested: bool,   // the Open button was clicked (main opens the picker)
    wall_scroll_held: bool, // an on-screen wall scroll arrow is held down
    scanning: bool,         // a folder is being picked/scanned on a worker thread
    focus: Option<usize>, // currently-focused tile
    focus_t: f32,         // 0 = wall, 1 = focused (animated)
    lb_zoom: f32,         // lightbox zoom (1 = fit; wheel zooms the focused item)
    lb_pan: [f32; 2],     // lightbox pan offset in NDC (drag moves a zoomed item)
    video: Option<crate::video::Player>, // playing the focused video tile, if any
    video_for: Option<usize>,            // which tile self.video belongs to
    last_activity: Instant,              // last pointer activity — video controls auto-hide on idle
    fullscreen_requested: bool,          // a control asked to toggle fullscreen (main polls it)
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
                    let (rgba, w, h) = decode(&job.source, job.full);
                    if result_tx
                        .send(Loaded {
                            index: job.index,
                            rgba,
                            w,
                            h,
                            gen: job.gen,
                            full: job.full,
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
            size: INSTANCE_CAP * size_of::<Instance>() as u64,
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
        let post = crate::post::Post::new(&device, &queue, config.format, size.width, size.height);

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

        // Full-resolution texture for the focused photo: one reusable FULL_PX layer (~16 MB). It's
        // a 1-layer 2d-array so the existing lightbox pipeline (which samples texture_2d_array) can
        // bind it as an alternate group(0) with no extra pipeline.
        let full_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("full-res"),
            size: wgpu::Extent3d {
                width: FULL_PX,
                height: FULL_PX,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let full_view = full_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let full_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("full-bg"),
            layout: &tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&full_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
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
            full_tex,
            full_bg,
            full_for: None,
            full_pending: None,
            full_extent: [1.0, 1.0],
            post,
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
            bank: 0.0,
            input_dir: 0.0,
            scroll_max,
            cam_dist: BASE_DIST,
            cam_dist_target: BASE_DIST,
            pan_y: 0.0,
            pointer_ndc: [0.0, 0.0],
            last_vp: [0.0, 0.0],
            drag_mode: DragMode::None,
            drag_last_x: 0.0,
            drag_last_y: 0.0,
            drag_moved: false,
            open_requested: false,
            wall_scroll_held: false,
            scanning: false,
            focus: None,
            focus_t: 0.0,
            lb_zoom: 1.0,
            lb_pan: [0.0, 0.0],
            video: None,
            video_for: None,
            last_activity: Instant::now(),
            fullscreen_requested: false,
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
            self.post
                .resize(&self.device, &self.queue, size.width, size.height);
            self.upload_camera();
        }
    }

    /// Mouse wheel: in the lightbox it zooms the focused item; on the wall, vertical = camera
    /// zoom, horizontal (trackpad) = pan. Web feel.
    pub fn wheel(&mut self, dx: f32, dy: f32) {
        if self.focus.is_some() {
            // Zoom the focused item toward the cursor, 1.2× per wheel step (matches the web). 1→8×.
            let old = self.lb_zoom;
            let factor = if dy < 0.0 { 1.2 } else { 1.0 / 1.2 };
            self.lb_zoom = (self.lb_zoom * factor).clamp(1.0, 8.0);
            let ratio = self.lb_zoom / old;
            // Keep the point under the cursor fixed as the image scales about it.
            let p = self.pointer_ndc;
            self.lb_pan[0] = p[0] - (p[0] - self.lb_pan[0]) * ratio;
            self.lb_pan[1] = p[1] - (p[1] - self.lb_pan[1]) * ratio;
            self.clamp_lb_pan();
            return;
        }
        if dx.abs() > dy.abs() {
            self.velocity += dx * 0.03;
        } else {
            self.cam_dist_target = (self.cam_dist_target + dy * 0.01).clamp(MIN_DIST, MAX_DIST);
        }
    }

    /// Keep the lightbox pan within the zoomed item's bounds (no pan when fit).
    fn clamp_lb_pan(&mut self) {
        if self.lb_zoom <= 1.0 {
            self.lb_pan = [0.0, 0.0];
            return;
        }
        // The fitted image is ~0.92 of the screen half-extent; at zoom z it overflows by ~(z·0.92−1)
        // on each side. Allow panning that far so you can reach every edge (but not into the void).
        let m = (self.lb_zoom * 0.92 - 1.0).max(0.0);
        self.lb_pan[0] = self.lb_pan[0].clamp(-m, m);
        self.lb_pan[1] = self.lb_pan[1].clamp(-m, m);
    }

    /// Reset zoom/pan — called whenever the focused item changes.
    fn reset_lb_view(&mut self) {
        self.lb_zoom = 1.0;
        self.lb_pan = [0.0, 0.0];
    }

    /// Aspect (w/h) of the focused image — full-res if loaded, else the thumbnail. None for videos
    /// or nothing decoded yet.
    fn focused_aspect(&self) -> Option<f32> {
        let idx = self.focus?;
        if self.full_for == Some(idx) {
            return Some(self.full_extent[0] / self.full_extent[1]);
        }
        match self.resident.get(&idx) {
            Some(Tile::Ready { aspect, .. }) => Some(*aspect),
            _ => None,
        }
    }

    /// The focused image's on-screen rect in NDC (x, y bottom-left, w, h), with zoom + pan applied.
    fn lightbox_rect_ndc(&self) -> Option<[f32; 4]> {
        let aspect = self.focused_aspect()?;
        let sa = self.config.width as f32 / self.config.height.max(1) as f32;
        let margin = 0.92;
        let (fitw, fith) = if aspect > sa {
            (2.0 * margin, 2.0 * margin * sa / aspect)
        } else {
            (2.0 * margin * aspect / sa, 2.0 * margin)
        };
        let (qw, qh) = (fitw * self.lb_zoom, fith * self.lb_zoom);
        Some([-qw / 2.0 + self.lb_pan[0], -qh / 2.0 + self.lb_pan[1], qw, qh])
    }

    /// True if a screen-space pixel falls on the focused image (vs the empty/dim area).
    fn click_on_lightbox_image(&self, x: f32, y: f32) -> bool {
        let Some(r) = self.lightbox_rect_ndc() else {
            return false;
        };
        let nx = x / self.config.width.max(1) as f32 * 2.0 - 1.0;
        let ny = 1.0 - y / self.config.height.max(1) as f32 * 2.0;
        nx >= r[0] && nx <= r[0] + r[2] && ny >= r[1] && ny <= r[1] + r[3]
    }

    /// Pointer pressed (button: 0 left, 1 middle, 2 right). Bottom band scrubs; middle/right
    /// grab-pan; left drags to scroll (or clicks to select).
    pub fn pointer_down(&mut self, button: u8, x: f32, y: f32) {
        self.drag_last_x = x;
        self.drag_last_y = y;
        self.drag_moved = false;
        self.last_activity = Instant::now();
        // The Open button (top-left toolbar).
        if button == 0 && x >= BTN_X && x <= BTN_X + BTN_W && y >= BTN_Y && y <= BTN_Y + BTN_H {
            self.open_requested = true;
            self.drag_mode = DragMode::None;
            return;
        }
        // Video controls bar (when a video is focused + controls shown).
        if button == 0 && self.video.is_some() && self.video_controls_visible() {
            let ui = self.video_ui();
            if hit(ui.bar, x, y) {
                self.drag_mode = DragMode::None; // a click on the bar never closes the lightbox
                if hit(ui.play, x, y) {
                    self.video_command(&["cycle", "pause"]);
                } else if hit(ui.audio, x, y) {
                    self.video_command(&["cycle", "aid"]);
                } else if hit(ui.subs, x, y) {
                    self.video_command(&["cycle", "sid"]);
                } else if hit(ui.full, x, y) {
                    self.fullscreen_requested = true;
                } else if hit([ui.seek[0], ui.bar[1], ui.seek[2], ui.bar[3]], x, y) {
                    self.drag_mode = DragMode::Seek; // grab the seek bar (drag to scrub)
                    self.seek_to_x(x);
                }
                return;
            }
        }
        // Edge arrow buttons. Focused: prev/next item. On the wall: hold to scroll left/right.
        if button == 0 && (self.focus.is_some() || self.scroll_max > 0.0) {
            let (prev, next) = self.arrow_rects();
            let on_prev = hit(prev, x, y);
            let on_next = hit(next, x, y);
            if on_prev || on_next {
                self.drag_mode = DragMode::None;
                let dir = if on_prev { -1.0 } else { 1.0 };
                if self.focus.is_some() {
                    self.navigate(dir as i64);
                } else {
                    self.input_dir = dir;
                    self.wall_scroll_held = true;
                }
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
        // Track the cursor in NDC (y up) every move so wheel-zoom can center on it.
        let w0 = self.config.width.max(1) as f32;
        let h0 = self.config.height.max(1) as f32;
        self.pointer_ndc = [x / w0 * 2.0 - 1.0, 1.0 - y / h0 * 2.0];
        self.last_activity = Instant::now(); // any motion un-hides the video controls
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
            DragMode::Seek => self.seek_to_x(x),
            DragMode::Scrub => self.scrub_to(x),
            DragMode::Pan => {
                let vph = self.viewport_h();
                self.pan_y = (self.pan_y + dy / h * vph).clamp(-PAN_Y_MAX, PAN_Y_MAX);
                if self.focus.is_none() {
                    self.scroll_x = (self.scroll_x - dx / h * vph).clamp(0.0, max);
                }
            }
            DragMode::Scroll => {
                if dx.abs() > 2.0 || dy.abs() > 2.0 {
                    self.drag_moved = true;
                }
                if self.focus.is_none() {
                    let vpw = self.viewport_h() * (self.config.width.max(1) as f32 / h);
                    let world = dx / h * vpw * DRAG_GAIN;
                    self.scroll_x = (self.scroll_x - world).clamp(0.0, max);
                    // velocity (for bank + release fling) is measured from real motion in update().
                } else {
                    // Lightbox: drag pans the (zoomed) focused item. NDC: +x right, +y up; screen
                    // y grows down, so negate. A drag never closes the lightbox (only a click does).
                    let w = self.config.width.max(1) as f32;
                    self.lb_pan[0] += dx / w * 2.0;
                    self.lb_pan[1] -= dy / h * 2.0;
                    self.clamp_lb_pan();
                }
            }
            DragMode::None => {}
        }
    }

    /// Pointer released: a left press with no drag is a click → select / deselect.
    pub fn pointer_up(&mut self, _button: u8) {
        if self.wall_scroll_held {
            self.input_dir = 0.0; // stop the held edge-arrow scroll
            self.wall_scroll_held = false;
        }
        let mode = self.drag_mode;
        self.drag_mode = DragMode::None;
        if mode == DragMode::Scroll && !self.drag_moved {
            if self.focus.is_some() {
                // Like the web: a click closes only on the empty/dim area — clicking the image
                // itself does nothing (so you can't accidentally close while interacting with it).
                if !self.click_on_lightbox_image(self.drag_last_x, self.drag_last_y) {
                    self.recenter_on_focus(); // leave the wall on the photo you were viewing
                    self.focus = None;
                }
            } else if let Some(i) = self.pick(self.drag_last_x, self.drag_last_y) {
                self.focus = Some(i);
                self.reset_lb_view();
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
        // velocity (→ lean) is measured from the real motion in update(), like a left-drag.
    }

    fn viewport_h(&self) -> f32 {
        2.0 * (FOV_Y * 0.5).tan() * self.cam_dist
    }

    /// World-space size for the focused video quad: a 16:9 rect (mpv renders into 1280×720) fitted
    /// to the viewport at FOCUS_DIST, so a focused video fills the view like a focused photo.
    fn video_fill_size(&self) -> [f32; 2] {
        let screen_aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let vph = 2.0 * (FOV_Y * 0.5).tan() * FOCUS_DIST;
        let vpw = vph * screen_aspect;
        let va = 16.0 / 9.0;
        let m = 0.96; // small margin
        if va > screen_aspect {
            [vpw * m, vpw * m / va]
        } else {
            [vph * m * va, vph * m]
        }
    }

    /// Pixel layout of the video controls bar.
    fn video_ui(&self) -> VideoUi {
        let w = self.config.width as f32;
        let h = self.config.height as f32;
        let by = h - VCTL_H;
        VideoUi {
            bar: [0.0, by, w, VCTL_H],
            play: [16.0, by + 14.0, 32.0, 32.0],
            seek: [180.0, by + 26.0, (w - 392.0).max(40.0), 8.0],
            audio: [w - 200.0, by + 15.0, 60.0, 30.0],
            subs: [w - 134.0, by + 15.0, 50.0, 30.0],
            full: [w - 78.0, by + 15.0, 44.0, 30.0],
        }
    }

    /// (position, duration, paused) of the playing video, if any.
    fn video_state(&self) -> Option<(f64, f64, bool)> {
        let v = self.video.as_ref()?;
        Some((v.position(), v.duration(), v.paused()))
    }

    /// Video controls show when a video is focused and there's been recent pointer activity (or
    /// it's paused) — they auto-hide after a few idle seconds, like the web player.
    fn video_controls_visible(&self) -> bool {
        if self.focus.is_none() || self.video.is_none() {
            return false;
        }
        let paused = self.video_state().map(|(_, _, p)| p).unwrap_or(false);
        self.last_activity.elapsed().as_secs_f32() < 2.5 || paused
    }

    /// Seek the video to the fraction of the seek track at pixel x.
    fn seek_to_x(&self, x: f32) {
        if let (Some(v), Some((_, dur, _))) = (self.video.as_ref(), self.video_state()) {
            if dur > 0.0 {
                let s = self.video_ui().seek;
                let frac = ((x - s[0]) / s[2]).clamp(0.0, 1.0);
                v.seek(frac as f64 * dur);
            }
        }
    }

    pub fn take_fullscreen_request(&mut self) -> bool {
        std::mem::take(&mut self.fullscreen_requested)
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
                // Left-drag and scrubber both own scroll_x directly; measure the real speed so the
                // wall leans into the motion (the scrubber drives the lean too, like the web).
                DragMode::Scroll | DragMode::Scrub => {
                    let measured = (self.scroll_x - self.prev_scroll_x) / dt.max(1e-4);
                    self.velocity += (measured - self.velocity) * 0.35; // low-pass → stable bank
                    self.velocity = self.velocity.clamp(-MAX_SPEED, MAX_SPEED);
                }
                DragMode::Pan | DragMode::Seek => self.velocity = 0.0,
                DragMode::None => {
                    if self.input_dir != 0.0 {
                        self.velocity = (self.velocity + self.input_dir * ACCEL * dt)
                            .clamp(-MAX_SPEED, MAX_SPEED);
                    } else {
                        self.velocity *= 0.045_f32.powf(dt); // momentum decay (matches web — eases out slowly)
                        if self.velocity.abs() < 0.002 {
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
        // Ease the wall's lean toward the velocity-driven target so it flattens gradually (rather
        // than snapping flat the instant you stop), matching the web's banking.
        let bank_target = if self.focus.is_some() {
            0.0
        } else {
            (self.velocity.signum() * self.velocity.abs().sqrt() * BANK_GAIN).clamp(-BANK_MAX, BANK_MAX)
        };
        self.bank += (bank_target - self.bank) * (6.0 * dt).min(1.0);

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

        // Zoom toward the cursor: as the viewport shrinks/grows with the zoom, shift the wall so the
        // world point under the pointer stays put (matches the web). Only on the wall, not focused.
        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let vph = self.viewport_h();
        let vpw = vph * aspect;
        if self.focus.is_none() && self.last_vp[0] > 0.0 {
            let dw = self.last_vp[0] - vpw;
            let dh = self.last_vp[1] - vph;
            let max = self.scroll_max.max(0.0);
            self.scroll_x = (self.scroll_x + self.pointer_ndc[0] * dw * 0.5).clamp(0.0, max);
            self.pan_y = (self.pan_y + self.pointer_ndc[1] * dh * 0.5).clamp(-PAN_Y_MAX, PAN_Y_MAX);
            self.prev_scroll_x = self.scroll_x; // zoom shift isn't a drag — don't let it bank
        }
        self.last_vp = [vpw, vph];

        // --- drain finished decodes: assign a layer + upload, or drop if no longer wanted ---
        // Cap GPU uploads per frame: a fast scroll can finish many decodes at once, and uploading
        // them all in one frame hitches. Spread them over frames — the rest stay queued.
        let mut uploads = 0;
        while uploads < MAX_UPLOADS_PER_FRAME {
            let Ok(res) = self.result_rx.try_recv() else {
                break;
            };
            // Full-res lightbox decodes are off the thumbnail throttle (one at a time, not pooled).
            if res.full {
                self.apply_full(res);
                continue;
            }
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
                    full: false,
                });
            }
        }

        // Request a full-resolution decode of the focused photo for a crisp lightbox (the thumb
        // shows immediately; the full image swaps in when it arrives). Dropped on deselect.
        match self.focus {
            Some(f) if matches!(self.sources.get(f), Some(Source::File(_))) => {
                if self.full_pending != Some(f) && self.full_for != Some(f) {
                    self.full_pending = Some(f);
                    let _ = self.job_tx.send(Job {
                        index: f,
                        source: self.sources[f].clone(),
                        gen: self.generation,
                        full: true,
                    });
                }
            }
            _ => {
                self.full_pending = None;
                self.full_for = None;
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
                        full: false,
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

    /// Upload a finished full-resolution decode into full_tex — if it's still the focused photo.
    fn apply_full(&mut self, res: Loaded) {
        if self.full_pending == Some(res.index) {
            self.full_pending = None;
        }
        if res.gen != self.generation || self.focus != Some(res.index) {
            return; // library swapped or the user moved on before it finished
        }
        if res.rgba.is_empty() || res.w == 0 || res.h == 0 {
            return; // decode failed — keep showing the thumbnail
        }
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.full_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &res.rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * res.w),
                rows_per_image: Some(res.h),
            },
            wgpu::Extent3d {
                width: res.w,
                height: res.h,
                depth_or_array_layers: 1,
            },
        );
        self.full_extent = [res.w as f32 / FULL_PX as f32, res.h as f32 / FULL_PX as f32];
        self.full_for = Some(res.index);
    }

    fn rebuild_instances(&mut self) {
        // Draw every tile in the visible window: a dark placeholder "skeleton" at the default size
        // until the image decodes, then the image at its true aspect. This keeps the grid full and
        // evenly spaced (matching the web) instead of leaving holes where tiles haven't loaded.
        // Reflections first so they paint behind the photos (no depth buffer → paint order).
        let mut refl: Vec<Instance> = Vec::new();
        let mut placeholders: Vec<Instance> = Vec::new();
        let mut photos: Vec<Instance> = Vec::new();
        let (first, last) = self.window_cols();
        for col in first..=last {
            for row in 0..ROWS {
                let i = col as usize * ROWS + row;
                if i >= self.total {
                    continue;
                }
                let cx = col as f32 * CELL_X;
                // Bottom-align tiles to a shared row baseline (a "shelf"), so different-height
                // photos — and their reflections — line up, exactly like the web wall.
                let baseline = row_baseline(row);
                if let Some(Tile::Ready { layer, aspect, uv }) = self.resident.get(&i) {
                    let (w, h) = size_for(*aspect);
                    photos.push(Instance {
                        offset: [cx, baseline + h * 0.5],
                        size: [w, h],
                        layer: *layer,
                        uv_extent: *uv,
                        kind: 0,
                    });
                    // The bottom row sits on glass: a mirrored, fading copy hangs beneath it.
                    if row == ROWS - 1 {
                        refl.push(Instance {
                            offset: [cx, baseline - REFLECT_GAP - h * 0.5],
                            size: [w, h],
                            layer: *layer,
                            uv_extent: *uv,
                            kind: 1,
                        });
                    }
                } else {
                    // Not decoded yet → skeleton at the default tile size.
                    let (w, h) = size_for(DEFAULT_ASPECT);
                    placeholders.push(Instance {
                        offset: [cx, baseline + h * 0.5],
                        size: [w, h],
                        layer: 0,
                        uv_extent: [1.0, 1.0],
                        kind: 2,
                    });
                }
            }
        }
        // Paint order: reflections (behind), then skeletons, then photos.
        refl.extend(placeholders);
        refl.extend(photos);
        refl.truncate(INSTANCE_CAP as usize);
        self.num_instances = refl.len() as u32;
        if !refl.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&refl));
        }
    }

    fn tile_center(&self, i: usize) -> (f32, f32) {
        let col = (i / ROWS) as f32;
        let row = i % ROWS;
        let aspect = match self.resident.get(&i) {
            Some(Tile::Ready { aspect, .. }) => *aspect,
            _ => DEFAULT_ASPECT,
        };
        let (_w, h) = size_for(aspect);
        (col * CELL_X, row_baseline(row) + h * 0.5)
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

        // Bank: the eased lean (set in update from velocity), faded out as a tile is focused.
        let bank = self.bank * (1.0 - s);
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
        // Invert the baseline placement (row 0 on top → row ROWS-1 on the bottom).
        let row = ((ROWS as f32 - 1.0) * 0.5 - hit.y / CELL_Y).round();
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

    /// Forward an mpv command to the playing video (no-op when nothing is playing or the `video`
    /// feature is off). e.g. ["cycle","pause"], ["cycle","aid"], ["cycle","sid"].
    pub fn video_command(&self, args: &[&str]) {
        if let Some(v) = &self.video {
            v.command(args);
        }
    }

    /// Prev/next item in the lightbox (dir = -1 / +1).
    pub fn navigate(&mut self, dir: i64) {
        if let (Some(f), true) = (self.focus, self.total > 0) {
            self.focus = Some((f as i64 + dir).clamp(0, self.total as i64 - 1) as usize);
            self.reset_lb_view();
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
            // Position/help hint at the bottom — hidden for a playing video (its controls bar
            // occupies that space instead).
            if !(self.video.is_some() && self.video_controls_visible()) {
                v.push(crate::ui::Line {
                    text: format!("{} / {}   ·   click ‹ › or ← →   ·   Esc", idx + 1, self.total),
                    x: cx - 140.0,
                    y: self.config.height as f32 - 40.0,
                    size: 16.0,
                    color: [225, 225, 230, 235],
                });
            }
            if matches!(self.sources.get(idx), Some(Source::Video(_))) && !cfg!(feature = "video") {
                // This build has no libmpv linked — explain why the clip isn't playing.
                v.push(crate::ui::Line {
                    text: "▶  video — rebuild with  --features video  to play".into(),
                    x: cx - 230.0,
                    y: cy - 16.0,
                    size: 22.0,
                    color: [235, 235, 240, 255],
                });
            } else if self.focus_loading() {
                v.push(crate::ui::Line {
                    text: "Loading…".into(),
                    x: cx - 52.0,
                    y: cy - 20.0,
                    size: 30.0,
                    color: [235, 235, 240, 255],
                });
            }
            // Video controls bar labels: play/pause, current/total time, track buttons, and a
            // hover-time tooltip over the seek bar.
            if self.video.is_some() && self.video_controls_visible() {
                let ui = self.video_ui();
                let (pos, dur, paused) = self.video_state().unwrap_or((0.0, 0.0, false));
                v.push(crate::ui::Line {
                    text: if paused { "▶".into() } else { "❚❚".into() },
                    x: ui.play[0] + 7.0,
                    y: ui.play[1] + 4.0,
                    size: 20.0,
                    color: [240, 240, 245, 255],
                });
                v.push(crate::ui::Line {
                    text: format!("{} / {}", fmt_time(pos), fmt_time(dur)),
                    x: 58.0,
                    y: ui.bar[1] + 21.0,
                    size: 14.0,
                    color: [225, 225, 230, 235],
                });
                v.push(crate::ui::Line {
                    text: "Audio".into(),
                    x: ui.audio[0] + 9.0,
                    y: ui.audio[1] + 7.0,
                    size: 14.0,
                    color: [230, 230, 235, 235],
                });
                v.push(crate::ui::Line {
                    text: "Subs".into(),
                    x: ui.subs[0] + 7.0,
                    y: ui.subs[1] + 7.0,
                    size: 14.0,
                    color: [230, 230, 235, 235],
                });
                v.push(crate::ui::Line {
                    text: "Full".into(),
                    x: ui.full[0] + 8.0,
                    y: ui.full[1] + 7.0,
                    size: 14.0,
                    color: [230, 230, 235, 235],
                });
                // Hover-time over the seek bar.
                let w = self.config.width as f32;
                let h = self.config.height as f32;
                let px = (self.pointer_ndc[0] + 1.0) * 0.5 * w;
                let py = (1.0 - self.pointer_ndc[1]) * 0.5 * h;
                if dur > 0.0
                    && px >= ui.seek[0]
                    && px <= ui.seek[0] + ui.seek[2]
                    && py >= ui.bar[1]
                    && py <= ui.bar[1] + ui.bar[3]
                {
                    let f = ((px - ui.seek[0]) / ui.seek[2]).clamp(0.0, 1.0) as f64;
                    v.push(crate::ui::Line {
                        text: fmt_time(f * dur),
                        x: px - 18.0,
                        y: ui.bar[1] - 26.0,
                        size: 15.0,
                        color: [255, 255, 255, 255],
                    });
                }
            }
        } else {
            // On-screen left/right scroll arrows (hold to scroll), when the wall is scrollable.
            if self.scroll_max > 0.0 {
                let (prev, next) = self.arrow_rects();
                v.push(crate::ui::Line {
                    text: "‹".into(),
                    x: prev[0] + ARROW_W * 0.5 - 9.0,
                    y: prev[1] + ARROW_H * 0.5 - 30.0,
                    size: 46.0,
                    color: [235, 235, 240, 210],
                });
                v.push(crate::ui::Line {
                    text: "›".into(),
                    x: next[0] + ARROW_W * 0.5 - 9.0,
                    y: next[1] + ARROW_H * 0.5 - 30.0,
                    size: 46.0,
                    color: [235, 235, 240, 210],
                });
            }
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

    /// Fit the focused image to the screen (preserving aspect), fade it in, and write the lightbox
    /// uniform. Prefers the full-res texture once it's uploaded, else the streamed thumbnail.
    /// Returns which texture the render pass should bind (None for videos / nothing decoded yet).
    fn prepare_lightbox(&self) -> LbDraw {
        let Some(idx) = self.focus else {
            return LbDraw::None;
        };
        if matches!(self.sources.get(idx), Some(Source::Video(_))) {
            return LbDraw::None; // videos play via the video layer, not the lightbox
        }
        // (aspect, uv extent, layer, which bind group) — full-res if ready, else the thumb.
        let (aspect, uv, layer, draw) = if self.full_for == Some(idx) {
            let e = self.full_extent;
            (e[0] / e[1], e, 0.0, LbDraw::Full)
        } else if let Some(Tile::Ready { layer, aspect, uv }) = self.resident.get(&idx) {
            (*aspect, *uv, *layer as f32, LbDraw::Thumb)
        } else {
            return LbDraw::None; // still decoding
        };
        let w = self.config.width.max(1) as f32;
        let h = self.config.height.max(1) as f32;
        let screen_aspect = w / h;
        let margin = 0.92; // leave a border around the fitted image
        let (fitw, fith) = if aspect > screen_aspect {
            (2.0 * margin, 2.0 * margin * screen_aspect / aspect)
        } else {
            (2.0 * margin * aspect / screen_aspect, 2.0 * margin)
        };
        // Apply lightbox zoom + pan (NDC).
        let (qw, qh) = (fitw * self.lb_zoom, fith * self.lb_zoom);
        let u = LbUniform {
            rect: [
                -qw / 2.0 + self.lb_pan[0],
                -qh / 2.0 + self.lb_pan[1],
                qw,
                qh,
            ],
            uv_layer: [uv[0], uv[1], layer, smoothstep(self.focus_t)],
        };
        self.queue
            .write_buffer(&self.lb_buf, 0, bytemuck::cast_slice(&[u]));
        draw
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

        // Edge arrow button backgrounds. Focused: prev/next (fade in, hidden at the ends). On the
        // wall: left/right scroll buttons (when scrollable).
        let to_ndc = |r: [f32; 4]| [nx(r[0]), ny_top(r[1] + r[3]), nw(r[2]), nhh(r[3])];
        let (prev, next) = self.arrow_rects();
        if let Some(f) = self.focus {
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
        } else if self.scroll_max > 0.0 {
            rects.push(OverlayRect {
                rect: to_ndc(prev),
                color: [1.0, 1.0, 1.0, 0.10],
            });
            rects.push(OverlayRect {
                rect: to_ndc(next),
                color: [1.0, 1.0, 1.0, 0.10],
            });
        }

        // Bottom scrubber (only when scrollable and not focused): a taller track with evenly spaced
        // tick lines and a draggable thumb whose width shows how much of the library is on screen.
        if self.focus.is_none() && self.scroll_max > 0.0 {
            let (pad, track_w, thumb_w) = self.scrubber_geom();
            let bh = nhh(18.0); // taller bar
            let by = -1.0 + nhh(14.0);
            rects.push(OverlayRect {
                rect: [nx(pad), by, nw(track_w), bh],
                color: [1.0, 1.0, 1.0, 0.14],
            });
            // Tick lines across the track (one per column step, capped so we never overflow).
            let ticks = (self.total_cols.max(1) as usize).min(40);
            if ticks > 1 {
                let tw = nw(1.5);
                let th = nhh(10.0);
                let ty = -1.0 + nhh(18.0);
                for k in 0..=ticks {
                    let fx = k as f32 / ticks as f32;
                    let x = pad + (track_w - 1.5) * fx;
                    rects.push(OverlayRect {
                        rect: [nx(x), ty, tw, th],
                        color: [1.0, 1.0, 1.0, 0.18],
                    });
                }
            }
            let frac = (self.scroll_x / self.scroll_max).clamp(0.0, 1.0);
            let thumb_x = pad + (track_w - thumb_w) * frac;
            rects.push(OverlayRect {
                rect: [nx(thumb_x), by, nw(thumb_w), bh],
                color: [0.95, 0.96, 1.0, 0.9],
            });
        }

        // Video controls bar (bottom): background, seek track + fill + knob, and button chips.
        if self.video.is_some() && self.video_controls_visible() {
            let ui = self.video_ui();
            // pixel rect [x,y,w,h] (top-left origin) → overlay NDC rect.
            let r = |p: [f32; 4]| [nx(p[0]), ny_top(p[1] + p[3]), nw(p[2]), nhh(p[3])];
            rects.push(OverlayRect {
                rect: r(ui.bar),
                color: [0.0, 0.0, 0.0, 0.55],
            });
            let (pos, dur, _) = self.video_state().unwrap_or((0.0, 0.0, false));
            let frac = if dur > 0.0 {
                (pos / dur).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            rects.push(OverlayRect {
                rect: r(ui.seek),
                color: [1.0, 1.0, 1.0, 0.22],
            });
            rects.push(OverlayRect {
                rect: r([ui.seek[0], ui.seek[1], ui.seek[2] * frac, ui.seek[3]]),
                color: [0.35, 0.70, 1.0, 0.95],
            });
            rects.push(OverlayRect {
                rect: r([
                    ui.seek[0] + ui.seek[2] * frac - 5.0,
                    ui.seek[1] - 5.0,
                    10.0,
                    ui.seek[3] + 10.0,
                ]),
                color: [1.0, 1.0, 1.0, 0.95],
            });
            for b in [ui.play, ui.audio, ui.subs, ui.full] {
                rects.push(OverlayRect {
                    rect: r(b),
                    color: [1.0, 1.0, 1.0, 0.12],
                });
            }
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

        // The lightbox backdrop blur/dim ramps in with the focus animation.
        let mix = smoothstep(self.focus_t);
        self.post.set_params(&self.queue, mix, 0.42);

        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        // Pass 1: the wall (tiles + reflections) → the offscreen scene texture.
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.post.scene_view(),
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
        }

        // Passes 2–3: blur the scene for the backdrop (only while the lightbox is open).
        if mix > 0.001 {
            self.post.record_blur(&mut enc);
        }

        // Pass 4: composite the backdrop (sharp wall ↔ blurred+dark) to the swapchain, then draw
        // the focused video / image and the 2D overlay + text on top.
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present-pass"),
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
            self.post.draw_composite(&mut rp);
            // Playing video draws over its (focused) tile, sized to fill the focused view (16:9,
            // the mpv render aspect), with the same zoom/pan as a focused photo.
            if let (Some(v), Some(idx)) = (&self.video, self.focus) {
                let (cx, cy) = self.tile_center(idx);
                let [sw, sh] = self.video_fill_size();
                let screen_aspect = self.config.width as f32 / self.config.height.max(1) as f32;
                let vph = 2.0 * (FOV_Y * 0.5).tan() * FOCUS_DIST;
                let ox = cx + self.lb_pan[0] * vph * screen_aspect * 0.5;
                let oy = cy + self.lb_pan[1] * vph * 0.5;
                let size = [sw * self.lb_zoom, sh * self.lb_zoom];
                v.draw(&mut rp, &self.camera_bg, [ox, oy], size, &self.queue);
            }
            // Lightbox: the focused image (full-res once ready, otherwise the streamed thumbnail).
            let lb_tex = match lb_visible {
                LbDraw::Full => Some(&self.full_bg),
                LbDraw::Thumb => Some(&self.tex_bg),
                LbDraw::None => None,
            };
            if let Some(tex_bg) = lb_tex {
                rp.set_pipeline(&self.lightbox_pipeline);
                rp.set_bind_group(0, tex_bg, &[]);
                rp.set_bind_group(1, &self.lb_bg, &[]);
                rp.draw(0..6, 0..1);
            }
            // Toolbar + scrubber + arrows (overlay rects [1..]; [0] dim is now done by composite).
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
fn decode(source: &Source, full: bool) -> (Vec<u8>, u32, u32) {
    let target = if full { FULL_PX } else { TILE_PX };
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
                    // Only downscale; never upscale a small image (no quality to gain, wastes VRAM).
                    let t = if img.width() > target || img.height() > target {
                        img.resize(target, target, image::imageops::FilterType::Triangle)
                            .to_rgba8()
                    } else {
                        img.to_rgba8()
                    };
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

/// Seconds → "M:SS" (or "H:MM:SS").
fn fmt_time(s: f64) -> String {
    if !s.is_finite() || s < 0.0 {
        return "0:00".into();
    }
    let t = s as u64;
    let (h, m, sec) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Tile (w, h) in world units for an image aspect: full row height, width capped at MAX_W (a very
/// wide photo keeps MAX_W and loses height instead). Matches the web wall's sizing.
fn size_for(aspect: f32) -> (f32, f32) {
    let mut w = TILE * aspect;
    let mut h = TILE;
    if w > MAX_W {
        w = MAX_W;
        h = MAX_W / aspect;
    }
    (w, h)
}

/// Shared bottom line (the "shelf") a row's tiles sit on: row 0 on top, row ROWS-1 on the bottom.
fn row_baseline(row: usize) -> f32 {
    ((ROWS as f32 - 1.0) * 0.5 - row as f32) * CELL_Y - TILE * 0.5
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
