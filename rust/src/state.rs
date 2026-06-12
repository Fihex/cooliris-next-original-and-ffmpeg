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
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

const TILE_PX: u32 = 512; // texture-array layer size (square thumbnails for now)
const ROWS: usize = 3;
const TILE: f32 = 1.0;
const GAP_X: f32 = 0.16;
const GAP_Y: f32 = 0.16;
const CELL_X: f32 = TILE + GAP_X;
const CELL_Y: f32 = TILE + GAP_Y;
const CAM_DIST: f32 = 6.0;
const FOV_Y: f32 = 0.9;

const POOL: u32 = 128; // resident texture-array layers (fixed VRAM ceiling: POOL * 1MB)
const KEEP_COLS: i64 = 16; // columns kept resident on each side of the camera
const MAX_INFLIGHT: usize = 8; // concurrent decodes (throttle, like the JS MAX_INFLIGHT)
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
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![2 => Float32x2, 3 => Float32x2, 4 => Uint32];

/// Where a tile's pixels come from — a file, or a generated placeholder when no folder is given.
#[derive(Clone)]
enum Source {
    File(PathBuf),
    Placeholder(usize),
}

struct Job {
    index: usize,
    source: Source,
}
struct Loaded {
    index: usize,
    rgba: Vec<u8>, // empty == decode failed
}

/// Per-resident-tile status.
enum Tile {
    Loading,
    Ready(u32), // assigned texture-array layer
    Failed,
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

    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    tex_bg: wgpu::BindGroup,
    tex: wgpu::Texture,

    // wall library + residency
    sources: Arc<Vec<Source>>,
    total: usize,
    total_cols: i64,
    resident: HashMap<usize, Tile>,
    free_layers: Vec<u32>,
    inflight: usize,

    // decode worker pool
    job_tx: Sender<Job>,
    result_rx: Receiver<Loaded>,

    // camera / scroll
    scroll_x: f32,
    velocity: f32,
    input_dir: f32,
    scroll_max: f32,
    last_frame: Instant,
    frame: u64,
}

impl State {
    pub async fn new(window: Arc<Window>) -> State {
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
        let sources = Arc::new(gather_sources());
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
                    let rgba = decode(&job.source);
                    if result_tx.send(Loaded { index: job.index, rgba }).is_err() {
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

        let state = State {
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
            camera_buf,
            camera_bg,
            tex_bg,
            tex,
            sources,
            total,
            total_cols,
            resident: HashMap::new(),
            free_layers,
            inflight: 0,
            job_tx,
            result_rx,
            scroll_x: 0.0,
            velocity: 0.0,
            input_dir: 0.0,
            scroll_max,
            last_frame: Instant::now(),
            frame: 0,
        };
        state.upload_camera();
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

    pub fn scroll(&mut self, delta: f32) {
        self.scroll_x = (self.scroll_x - delta).clamp(0.0, self.scroll_max.max(0.0));
        self.velocity = 0.0;
    }

    pub fn set_dir(&mut self, dir: f32) {
        self.input_dir = dir;
    }

    pub fn update(&mut self) {
        // --- scroll physics ---
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        const ACCEL: f32 = 14.0;
        const MAX_SPEED: f32 = 9.0;
        const DAMP: f32 = 6.0;
        if self.input_dir != 0.0 {
            self.velocity =
                (self.velocity + self.input_dir * ACCEL * dt).clamp(-MAX_SPEED, MAX_SPEED);
        } else {
            self.velocity *= (1.0 - DAMP * dt).max(0.0);
            if self.velocity.abs() < 0.001 {
                self.velocity = 0.0;
            }
        }
        let max = self.scroll_max.max(0.0);
        self.scroll_x = (self.scroll_x + self.velocity * dt).clamp(0.0, max);
        if self.scroll_x <= 0.0 || self.scroll_x >= max {
            self.velocity = 0.0;
        }

        // --- drain finished decodes: assign a layer + upload, or drop if no longer wanted ---
        while let Ok(res) = self.result_rx.try_recv() {
            self.inflight = self.inflight.saturating_sub(1);
            if !matches!(self.resident.get(&res.index), Some(Tile::Loading)) {
                continue; // evicted while in flight
            }
            if res.rgba.is_empty() {
                self.resident.insert(res.index, Tile::Failed);
            } else if self.in_window(res.index) {
                if let Some(layer) = self.free_layers.pop() {
                    self.upload_layer(layer, &res.rgba);
                    self.resident.insert(res.index, Tile::Ready(layer));
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
                (col < first || col > last) && !matches!(t, Tile::Loading)
            })
            .map(|(i, _)| *i)
            .collect();
        for i in evict {
            if let Some(Tile::Ready(layer)) = self.resident.remove(&i) {
                self.free_layers.push(layer);
            }
        }

        // --- dispatch new loads, nearest column first, throttled ---
        let center = (self.scroll_x / CELL_X).round() as i64;
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
                    });
                }
            }
        }

        self.rebuild_instances();
        self.upload_camera();

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

    fn window_cols(&self) -> (i64, i64) {
        let center = (self.scroll_x / CELL_X).round() as i64;
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

    fn upload_layer(&self, layer: u32, rgba: &[u8]) {
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
                bytes_per_row: Some(4 * TILE_PX),
                rows_per_image: Some(TILE_PX),
            },
            wgpu::Extent3d {
                width: TILE_PX,
                height: TILE_PX,
                depth_or_array_layers: 1,
            },
        );
    }

    fn rebuild_instances(&mut self) {
        let mut inst: Vec<Instance> = Vec::with_capacity(self.resident.len());
        for (&i, t) in &self.resident {
            if let Tile::Ready(layer) = *t {
                let col = (i / ROWS) as f32;
                let row = (i % ROWS) as f32;
                inst.push(Instance {
                    offset: [col * CELL_X, (row - 1.0) * CELL_Y],
                    size: [TILE, TILE],
                    layer,
                });
            }
        }
        self.num_instances = inst.len() as u32;
        if !inst.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&inst));
        }
    }

    fn upload_camera(&self) {
        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let eye = Vec3::new(self.scroll_x, 0.0, CAM_DIST);
        let target = Vec3::new(self.scroll_x, 0.0, 0.0);
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);
        let proj = Mat4::perspective_rh(FOV_Y, aspect, 0.1, 100.0);
        let u = CameraUniform {
            view_proj: (proj * view).to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::cast_slice(&[u]));
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
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
        }
        self.queue.submit(std::iter::once(enc.finish()));
        frame.present();
        Ok(())
    }
}

/// Decode + downscale one tile's pixels (runs on a worker thread). Empty Vec == failure.
fn decode(source: &Source) -> Vec<u8> {
    match source {
        Source::File(p) => match image::open(p) {
            Ok(img) => img
                .resize_exact(TILE_PX, TILE_PX, image::imageops::FilterType::Triangle)
                .to_rgba8()
                .into_raw(),
            Err(e) => {
                log::warn!("skip {p:?}: {e}");
                Vec::new()
            }
        },
        Source::Placeholder(i) => placeholder(*i),
    }
}

/// Build the tile library from the first CLI arg (a folder of images), or placeholders.
fn gather_sources() -> Vec<Source> {
    let Some(dir) = std::env::args().nth(1) else {
        log::info!("no folder given (pass one as the first argument) — showing placeholders");
        return (0..24).map(Source::Placeholder).collect();
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref(),
                Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
            )
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        log::info!("no images in {dir:?} — showing placeholders");
        return (0..24).map(Source::Placeholder).collect();
    }
    paths.into_iter().map(Source::File).collect()
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
