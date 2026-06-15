// GPU state + virtualized, streamed wall.
//
// This is the bounded-memory core. We never hold the whole library on the GPU: a fixed pool of
// texture-array layers is recycled as tiles scroll in and out of a window around the camera.
// Decoding runs on a worker thread pool (files read directly — no IPC, no protocol), and the
// main thread only assigns a free layer + uploads when pixels come back. Scroll a 16k-photo
// library and GPU/CPU stay flat — by construction, not by fighting a garbage collector.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use glam::{Mat4, Vec3, Vec4};

use crate::components::{
    self, Filter, MenuKind, OverlayRect, SortMode, TrackMenu, UiAction, UiCtx, VideoCtx,
};
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

// Layout + motion tuned to match the web wall (libmpv/src/wall/WallScene.ts).
const TILE_PX: u32 = 512; // texture-array layer size (images resized to fit, preserving aspect)
const FULL_PX: u32 = 2048; // full-res size decoded for the focused photo (crisp lightbox)
const ROWS: usize = 3;
const TILE: f32 = 1.0; // row height (ROW_H)
const MAX_W: f32 = 1.55; // widest a landscape tile may get
const GAP_X: f32 = 0.10; // equal horizontal/vertical world gap
const GAP_Y: f32 = 0.10;
const CELL_X: f32 = MAX_W + GAP_X; // column pitch (1.71)
const CELL_Y: f32 = TILE + GAP_Y;
const DEFAULT_ASPECT: f32 = 1.4; // assumed aspect before a tile's image has decoded
const REFLECT_GAP: f32 = 0.09; // gap between a photo and its mirrored reflection
const FOV_Y: f32 = 45.0 * std::f32::consts::PI / 180.0; // 45°, like the web camera
const BASE_DIST: f32 = 7.2; // default camera distance (wheel zooms between MIN..MAX)
const MIN_DIST: f32 = 4.5;
const MAX_DIST: f32 = 13.0;
const FOCUS_DIST: f32 = 5.6; // distance when a tile is focused
const CAM_Y: f32 = 0.0; // camera centred on the wall (rotation/bank pivots around the centre)
const BANK_GAIN: f32 = 0.22; // sqrt(|vel|) → bank radians
const BANK_MAX: f32 = 0.5;
const PAN_Y_MAX: f32 = 1.7; // vertical grab-pan limit
const DRAG_GAIN: f32 = 0.6; // left-drag scroll sensitivity
const SCRUB_ZONE_PX: f32 = 44.0; // bottom band that acts as the scrubber
const ARROW_W: f32 = 68.0; // lightbox prev/next buttons (vertically centered on each edge)
const ARROW_H: f32 = 104.0;
const ARROW_MARGIN: f32 = 18.0;
const ACCEL: f32 = 22.0; // arrow-key acceleration (world units / s²)
const MAX_SPEED: f32 = 11.0;
const ANIM_SPEED: f32 = 4.0; // focus in/out transition speed (1 / seconds)

const POOL: u32 = 128; // resident texture-array layers (fixed VRAM ceiling: POOL * 1MB)
const INSTANCE_CAP: u64 = POOL as u64 * 2; // photos + their reflections (bottom row adds ~POOL/3)
const KEEP_COLS: i64 = 16; // columns kept resident on each side of the camera
const MAX_INFLIGHT: usize = 32; // concurrent decodes in flight (deep queue keeps every worker fed)
const MAX_UPLOADS_PER_FRAME: usize = 10; // GPU texture uploads/frame (drains decode bursts faster)
const WALL_GIF_PROV: u32 = 128; // provisional per-frame decode size before packing into the atlas
const WALL_GIF_FRAMES: usize = 256; // cap frames (a 16×16 atlas grid in the 512 layer)
const MAX_GIF_DECODES: usize = 3; // concurrent GIF decodes (decoding many at once stutters)
const GIF_FAST_VEL: f32 = 4.0; // don't start new GIF decodes while scrolling faster than this
// Wall GIFs are sampled at one global low rate (web wall: base 24fps − 20 skip = 4fps) — each
// shows its time-correct frame, so the tempo is right while uploads stay cheap.
const GIF_WALL_TICK_S: f32 = 1.0 / 4.0;
// Decode threads are chosen at runtime from the CPU (see State::new). DecodeBudget caps peak RAM,
// so the thread count is purely about throughput — a fixed 4 was far too few on modern machines.

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
    uv_offset: [f32; 2], // sub-rect origin within the layer (animated GIFs sample one atlas cell)
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
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    2 => Float32x2, 3 => Float32x2, 4 => Uint32, 5 => Float32x2, 6 => Uint32, 7 => Float32x2];

const OVERLAY_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
const OVERLAY_CAP: u64 = 512; // wall overlay (dim/scrubber/arrows/ticks/title boxes) + custom UI rects (incl. the Open dialog's dashed drop-zone border)
const OVERLAY_SHADER: &str = r#"
struct In { @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>, @location(2) round: vec4<f32> };
struct V {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) round: vec4<f32>,
};
@vertex
fn vs(@builtin(vertex_index) vi: u32, in: In) -> V {
    var c = array<vec2<f32>, 6>(
        vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let q = c[vi];
    let p = in.rect.xy + q * in.rect.zw;
    var out: V;
    out.clip = vec4(p, 0.0, 1.0);
    out.color = in.color;
    out.uv = q;
    out.round = in.round;
    return out;
}
// Colours are authored in sRGB (hex), but the swapchain is an sRGB format that re-encodes the
// shader output — so convert sRGB→linear here, otherwise mid-tones render washed-out/gray.
fn s2l(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}
@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    let r = in.round.x;            // corner radius (px)
    if (r <= 0.0) {
        // round.w > 0 on a sharp rect = vertical ALPHA gradient (opaque at top → transparent at the
        // bottom): the top-bar's black→transparent fade, smooth (no banding).
        let a = select(in.color.a, in.color.a * in.uv.y, in.round.w > 0.0);
        return vec4<f32>(s2l(in.color.rgb), a);
    }
    let size = in.round.yz;        // rect size (px)
    let p = in.uv * size - size * 0.5;            // position from the rect centre
    let q = abs(p) - (size * 0.5 - vec2<f32>(r)); // rounded-box SDF
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let alpha = in.color.a * (1.0 - smoothstep(-0.75, 0.75, d)); // 1.5px edge AA
    // Vertical gradient (round.w = strength, 0 = flat): lighter at the top, darker at the bottom.
    let g = in.round.w;
    let col = in.color.rgb * mix(1.0 - g, 1.0 + g, in.uv.y);
    return vec4<f32>(s2l(col), alpha);
}
"#;

/// Lightbox: the focused image drawn as a fitted, centered screen-space quad over a dimmed wall.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LbUniform {
    rect: [f32; 4],     // NDC x, y (bottom-left), w, h
    uv_layer: [f32; 4], // uv.x, uv.y (extent), layer, alpha
    uv_off: [f32; 4],   // atlas sub-rect origin (uv.x, uv.y) for animated-GIF cells; .zw unused
}
const LIGHTBOX_SHADER: &str = r#"
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var samp: sampler;
struct LB { rect: vec4<f32>, uv_layer: vec4<f32>, uv_off: vec4<f32> };
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
    out.uv = lb.uv_off.xy + vec2(q.x, 1.0 - q.y) * (lb.uv_layer.xy - vec2(0.5 / 512.0, 0.5 / 512.0));
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
    Audio(PathBuf), // music: cover-art thumbnail, plays via mpv on focus
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

/// One decoded GIF frame: pixels + size + how long to show it (seconds).
struct GifFrame {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    delay: f32,
}
/// All frames of the focused GIF (delivered from a worker thread).
struct GifMsg {
    index: usize,
    gen: u64,
    frames: Vec<GifFrame>,
}
/// The focused (opened) GIF: full-res frames cycled into full_tex at full speed.
struct GifAnim {
    index: usize,
    frames: Vec<GifFrame>,
    cur: usize,
    t: f32,
}

/// A GIF packed into one 512×512 atlas (a grid of frames) for the wall. Uploaded to the tile's
/// layer once; animation is just a per-frame UV-offset change — so an animated wall GIF costs no
/// extra RAM and no per-frame uploads (the frames live in the pool layer it already owns).
struct GifAtlas {
    rgba: Vec<u8>,    // 512×512×4 packed grid
    grid: u32,        // cells per row/column
    cell: u32,        // cell size in px (512 / grid)
    fw: u32,          // frame size within a cell (≤ cell, preserves aspect)
    fh: u32,
    delays: Vec<f32>, // per-frame delay (seconds)
    total: f32,       // loop duration
}
/// Decoded wall GIF delivered from a worker (atlas is None on failure).
struct WallGifMsg {
    index: usize,
    gen: u64,
    atlas: Option<GifAtlas>,
}
/// Live wall-GIF state — metadata only; the frames are packed in the tile's layer.
struct GifAtlasAnim {
    grid: u32,
    cell: u32,
    fw: u32,
    fh: u32,
    delays: Vec<f32>,
    total: f32,
    start: Instant,
    cur: usize,
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

/// Which picker the Open menu asked for (polled by `main`, which owns the native dialog threads).
#[derive(Clone, Copy, PartialEq)]
pub enum OpenKind {
    Files,
    Folder,
    Json, // "From JSON…" — pick a .json manifest of media paths
}

/// What a held pointer is doing — matches the web wall: left-drag scrolls, right/middle-drag
/// grab-pans, the bottom band scrubs.
#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    None,
    Scroll,
    Pan,
    Scrub,
    VideoSeek, // dragging the timeline scrubber
    VideoVol,  // dragging the volume slider
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
    icons: crate::icons::Icons,  // SVG UI icons (video controls, lightbox arrows/info)

    camera_buf: wgpu::Buffer,
    camera_bg: wgpu::BindGroup,
    tex_bg: wgpu::BindGroup,
    tex: wgpu::Texture,

    // wall library + residency
    all_sources: Arc<Vec<Source>>, // full scanned library (sorted); `sources` is the filtered view
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
    pjob_tx: Sender<Job>, // priority lane (focused full-res decode jumps the queue)
    result_rx: Receiver<Loaded>,

    // camera / scroll
    scroll_x: f32,
    prev_scroll_x: f32,         // last frame's scroll_x — measures real drag speed for banking
    scroll_target: Option<f32>, // eased scrub/slider target (smooth, derives the lean)
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
    open_request: Option<OpenKind>, // Open menu picked Files/Folder (main opens the native dialog)
    show_info: bool,              // info panel toggle (filename/path of the focused/hovered item)
    sort_mode: SortMode,          // current library sort order
    filter_kind: Filter,          // type filter (all / photos / videos / audio)
    open_menu: Option<MenuKind>,  // which toolbar dropdown is open
    track_menu: Option<TrackMenu>, // which video track-selection menu is open
    search: String,               // search query (filters the wall by filename)
    search_active: bool,          // the search box has keyboard focus (typing edits it)
    date_created: bool,           // Dates filter: false = Modified, true = Created
    date_from: String,            // "YYYY-MM-DD" lower bound (empty = unbounded)
    date_to: String,              // "YYYY-MM-DD" upper bound (empty = unbounded)
    date_active: u8,              // which date field takes keyboard: 0 none, 1 From, 2 To
    caret: usize,                 // caret char-index within the focused text field (search/date)
    view_dirty: bool,             // search/filter changed → rebuild the displayed view next frame
    slideshow: bool,              // auto-advance the focused item
    slideshow_t: Instant,         // last slideshow advance
    gif_anim: bool,               // Settings: animate GIFs on focus
    reflections: bool,            // Settings: draw the glass reflections
    show_titles: bool,            // Settings: filename label on every wall tile
    show_mem: bool,               // Settings: show the memory-usage readout (+ periodic log)
    mem_log: Instant,             // throttles the memory-usage log line
    wall_scroll_held: bool, // an on-screen wall scroll arrow is held down
    scanning: bool,         // a folder is being picked/scanned on a worker thread
    scan_count: usize,      // media files found so far during a scan (progress readout)
    focus: Option<usize>,      // currently-focused tile
    focus_t: f32,              // 0 = wall, 1 = focused (animated)
    hover_index: Option<usize>,         // tile under the cursor (wall only)
    hover_scales: HashMap<usize, f32>,  // per-tile eased zoom (smooth grow/shrink, no snapping)
    lb_zoom: f32,              // lightbox zoom (1 = fit; wheel zooms the focused item)
    lb_pan: [f32; 2],          // lightbox pan offset in NDC (drag moves a zoomed item)
    video: Option<crate::video::Player>, // playing the focused video tile, if any
    video_for: Option<usize>,            // which tile self.video belongs to
    video_pending: Option<usize>,        // a focused video/audio whose mpv start is deferred one frame
    volume: f64,                         // last-set volume, carried to each new video/track
    seek_preview: Option<f32>,           // scrubber fraction while dragging (knob tracks the cursor)
    gif: Option<GifAnim>,                // animating focused GIF (plays into full_tex)
    gif_pending: Option<usize>,          // GIF whose frames are being decoded on a worker
    gif_tx: Sender<GifMsg>,
    gif_rx: Receiver<GifMsg>,
    wall_gifs: HashMap<usize, GifAtlasAnim>, // animated GIF thumbnails on the wall (atlas per tile)
    wall_gif_pending: HashSet<usize>,    // wall GIFs whose atlas is being decoded
    wall_gif_tx: Sender<WallGifMsg>,
    wall_gif_rx: Receiver<WallGifMsg>,
    gif_tick: Instant,                   // global low-rate sample tick for wall GIFs
    last_activity: Instant,              // last pointer activity — video controls auto-hide on idle
    fullscreen_requested: bool,          // a control asked to toggle fullscreen (main polls it)
    last_frame: Instant,
    frame: u64,
    prev_inflight: usize, // to trim the heap when a decode burst finishes
}

impl State {
    pub async fn new(window: Arc<Window>, folder: Option<PathBuf>) -> State {
        let size = window.inner_size();

        // Pick the backend. On Windows, default to DX12 — wgpu's Vulkan path is markedly slower on
        // some NVIDIA setups (the GL path glitches D2Array textures), while DX12 is smooth and is
        // the recommended native backend there. Elsewhere use the fast native set. Override with
        // WGPU_BACKEND=(vulkan|dx12|gl) to compare.
        let backends = match std::env::var("WGPU_BACKEND").ok().as_deref() {
            Some("vulkan") => wgpu::Backends::VULKAN,
            Some("dx12") => wgpu::Backends::DX12,
            Some("gl") => wgpu::Backends::GL,
            _ if cfg!(target_os = "windows") => wgpu::Backends::DX12,
            _ => wgpu::Backends::PRIMARY,
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
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
        // Log the chosen GPU + backend — a software/integrated adapter is the usual cause of low fps.
        let ai = adapter.get_info();
        log::info!(
            "GPU: {} | backend {:?} | type {:?} | driver {} {}",
            ai.name, ai.backend, ai.device_type, ai.driver, ai.driver_info
        );
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
        // all_sources keeps the original scan order ("Default (as loaded)"); the displayed `sources`
        // is the filtered + sorted view, rebuilt on sort/filter/search. At startup: no filter/sort.
        let all_sources = Arc::new(gather_sources(folder.clone(), |_| {}));
        let sources = all_sources.clone();
        let total = sources.len();
        let total_cols = (total.div_ceil(ROWS)) as i64;
        let scroll_max = (total_cols - 1).max(0) as f32 * CELL_X;
        log::info!("library: {total} tiles ({total_cols} columns)");

        // --- decode worker pool ---
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
        // Priority lane: the focused full-res decode jumps ahead of wall-thumb streaming so an
        // opened photo reaches best quality fast (not after the streaming queue drains).
        let (pjob_tx, pjob_rx) = crossbeam_channel::unbounded::<Job>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<Loaded>();
        let (gif_tx, gif_rx) = crossbeam_channel::unbounded::<GifMsg>();
        let (wall_gif_tx, wall_gif_rx) = crossbeam_channel::unbounded::<WallGifMsg>();
        // Cap total in-flight decode memory so opening a folder of very large images doesn't spike
        // RSS to several GB (each full-res decode is w·h·4 bytes; 4 workers × a huge photo added up).
        let budget = Arc::new(DecodeBudget::new(1_100_000_000)); // ~1.1 GB
        // One decode thread per logical CPU (clamped). Decoding + thumbnailing is CPU-bound, so this
        // scales loading speed with the machine instead of a fixed 4; RAM stays capped by `budget`.
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(4, 12);
        log::info!("decode workers: {workers}");
        for _ in 0..workers {
            let job_rx = job_rx.clone();
            let pjob_rx = pjob_rx.clone();
            let result_tx = result_tx.clone();
            let budget = budget.clone();
            std::thread::spawn(move || {
                loop {
                    // Always take a priority (focused full-res) job first; otherwise block on either.
                    let job = match pjob_rx.try_recv() {
                        Ok(j) => j,
                        Err(_) => crossbeam_channel::select! {
                            recv(pjob_rx) -> j => match j { Ok(j) => j, Err(_) => break },
                            recv(job_rx) -> j => match j { Ok(j) => j, Err(_) => break },
                        },
                    };
                    let est = estimate_decode_bytes(&job.source);
                    budget.acquire(est);
                    let (rgba, w, h) = decode(&job.source, job.full);
                    budget.release(est);
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
        let mut ui = crate::ui::Ui::new(&device, &queue, config.format);
        // Measure the (static) toolbar labels once so buttons size + centre their text exactly.
        let mut label_w = std::collections::HashMap::new();
        for l in ["Open", "Slideshow", "Stop", "Fullscreen", "Settings", "Sort", "Filter", "Dates", "Dates \u{2022}"] {
            label_w.insert(l.to_string(), ui.text_width(l, 14.0));
        }
        // Dates-panel buttons render at size 13.
        for l in ["Modified", "Created", "Done", "Clear dates"] {
            label_w.insert(l.to_string(), ui.text_width(l, 13.0));
        }
        // Digits + colon at size 13 — so the seek-hover time tooltip centres exactly.
        for ch in "0123456789:".chars() {
            label_w.insert(ch.to_string(), ui.text_width(&ch.to_string(), 13.0));
        }
        // Open-media modal: buttons + the line-1 prompt at size 14.
        for l in ["Choose files\u{2026}", "Choose folder\u{2026}", "From JSON\u{2026}", "Drag & drop files or a folder here"] {
            label_w.insert(l.to_string(), ui.text_width(l, 14.0));
        }
        // The modal's line-2 hint renders at size 12.
        let l2 = "or click to choose a folder \u{00b7} images, videos, audio";
        label_w.insert(l2.to_string(), ui.text_width(l2, 12.0));
        components::set_label_widths(label_w);
        let icons = crate::icons::Icons::new(&device, &queue, config.format);
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
            icons,
            camera_buf,
            camera_bg,
            tex_bg,
            tex,
            all_sources,
            sources,
            total,
            total_cols,
            generation: 0,
            current_folder: folder,
            resident: HashMap::new(),
            free_layers,
            inflight: 0,
            job_tx,
            pjob_tx,
            result_rx,
            scroll_x: 0.0,
            prev_scroll_x: 0.0,
            scroll_target: None,
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
            open_request: None,
            show_info: false,
            sort_mode: SortMode::Default,
            filter_kind: Filter::All,
            open_menu: None,
            track_menu: None,
            search: String::new(),
            search_active: false,
            date_created: false,
            date_from: String::new(),
            date_to: String::new(),
            date_active: 0,
            caret: 0,
            view_dirty: false,
            slideshow: false,
            slideshow_t: Instant::now(),
            gif_anim: true,
            reflections: true,
            show_titles: false,
            show_mem: false,
            mem_log: Instant::now(),
            wall_scroll_held: false,
            scanning: false,
            scan_count: 0,
            focus: None,
            focus_t: 0.0,
            hover_index: None,
            hover_scales: HashMap::new(),
            lb_zoom: 1.0,
            lb_pan: [0.0, 0.0],
            video: None,
            video_for: None,
            video_pending: None,
            volume: 100.0,
            seek_preview: None,
            gif: None,
            gif_pending: None,
            gif_tx,
            gif_rx,
            wall_gifs: HashMap::new(),
            wall_gif_pending: HashSet::new(),
            wall_gif_tx,
            wall_gif_rx,
            gif_tick: Instant::now(),
            last_activity: Instant::now(),
            fullscreen_requested: false,
            last_frame: Instant::now(),
            frame: 0,
            prev_inflight: 0,
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

    /// Hover tooltip pill for the hovered tile — same look as the show-titles pills (centred on the
    /// tile's bottom, solid black box). Returns (name, box_x, box_y, box_w) in pixels. Suppressed
    /// when show-titles is on (every tile already has one).
    fn hover_label(&self) -> Option<(String, f32, f32, f32)> {
        // No hover pill while focused, while show-titles is on, or during a scan (the title text is
        // suppressed then, so the box would otherwise show empty).
        if self.focus.is_some() || self.show_titles || self.scanning {
            return None;
        }
        let i = self.hover_index?;
        let mut name = match self.sources.get(i)? {
            Source::File(p) | Source::Video(p) | Source::Audio(p) => {
                p.file_name().map(|s| s.to_string_lossy().into_owned())?
            }
            Source::Placeholder(_) => return None,
        };
        if name.chars().count() > 22 {
            name = name.chars().take(21).collect::<String>() + "\u{2026}";
        }
        let w = self.config.width as f32;
        let h = self.config.height as f32;
        // Follow the cursor: centre the pill horizontally on the mouse, floating just above it
        // (not pinned to the tile), so the name tracks where you're actually pointing.
        let mx = (self.pointer_ndc[0] + 1.0) * 0.5 * w;
        let my = (1.0 - self.pointer_ndc[1]) * 0.5 * h;
        let bw = name.chars().count() as f32 * 7.0 + 16.0;
        let bx = (mx - bw * 0.5).clamp(4.0, (w - bw - 4.0).max(4.0));
        let by = (my - 34.0).max(4.0);
        Some((name, bx, by, bw))
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
        // The custom UI (top bar, dropdowns, search, video controls) gets the click first. The
        // seek/volume sliders begin a drag so you can hold-and-scrub them.
        if button == 0 {
            if let Some(action) = components::hit_test(&self.ui_ctx(), x, y) {
                match action {
                    UiAction::VideoSeekFrac(f) => {
                        // Begin a scrub — the knob tracks the cursor; the seek lands on release.
                        self.drag_mode = DragMode::VideoSeek;
                        self.seek_preview = Some(f);
                    }
                    UiAction::VideoVolume(_) => {
                        self.drag_mode = DragMode::VideoVol;
                        self.apply_ui_action(action);
                    }
                    other => {
                        self.drag_mode = DragMode::None;
                        self.apply_ui_action(other);
                    }
                }
                return;
            }
        }
        // A click anywhere else dismisses an open menu / search / date focus.
        self.open_menu = None;
        self.search_active = false;
        self.date_active = 0;
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
                    self.scroll_target = None;
                }
                return;
            }
        }
        // A left-click on a focused video toggles pause instantly (controls bar + edge arrows were
        // handled above; the video is full-bleed, so there's no click-to-close to confuse it with).
        if button == 0 && self.focused_is_video() {
            self.video_command(&["cycle", "pause"]);
            self.drag_mode = DragMode::None;
            return;
        }
        let h = self.config.height as f32;
        if self.focus.is_none() && self.scroll_max > 0.0 && y > h - SCRUB_ZONE_PX {
            self.drag_mode = DragMode::Scrub;
            self.scrub_to(x);
        } else if button == 1 || button == 2 {
            self.drag_mode = DragMode::Pan;
            self.scroll_target = None; // grab-pan owns scroll_x directly
        } else {
            self.drag_mode = DragMode::Scroll;
            self.velocity = 0.0;
            self.scroll_target = None; // left-drag owns scroll_x directly
        }
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        // Track the cursor in NDC (y up) every move so wheel-zoom can center on it.
        let w0 = self.config.width.max(1) as f32;
        let h0 = self.config.height.max(1) as f32;
        self.pointer_ndc = [x / w0 * 2.0 - 1.0, 1.0 - y / h0 * 2.0];
        self.last_activity = Instant::now(); // any motion un-hides the video controls
        if self.drag_mode == DragMode::None {
            // Hover (wall only): the tile under the cursor zooms in place + shows its name. Skip
            // when the cursor is over the custom UI (top bar, menus, video bar).
            self.hover_index = if self.focus.is_none()
                && !components::pointer_over_ui(&self.ui_ctx(), x, y)
            {
                self.pick(x, y)
            } else {
                None
            };
            return;
        }
        self.hover_index = None; // no hover while dragging
        let dx = x - self.drag_last_x;
        let dy = y - self.drag_last_y;
        self.drag_last_x = x;
        self.drag_last_y = y;
        let h = self.config.height.max(1) as f32;
        let max = self.scroll_max.max(0.0);
        match self.drag_mode {
            DragMode::Scrub => self.scrub_to(x),
            DragMode::Pan => {
                if self.focus.is_some() {
                    // Lightbox: middle/right-drag is the grab-to-move for the zoomed item (NDC:
                    // +x right, +y up; screen y grows down → negate).
                    let w = self.config.width.max(1) as f32;
                    self.lb_pan[0] += dx / w * 2.0;
                    self.lb_pan[1] -= dy / h * 2.0;
                    self.clamp_lb_pan();
                } else {
                    // Wall: free grab-pan (vertical + horizontal).
                    let vph = self.viewport_h();
                    self.pan_y = (self.pan_y + dy / h * vph).clamp(-PAN_Y_MAX, PAN_Y_MAX);
                    self.scroll_x = (self.scroll_x - dx / h * vph).clamp(0.0, max);
                }
            }
            DragMode::Scroll => {
                if dx.abs() > 2.0 || dy.abs() > 2.0 {
                    self.drag_moved = true;
                }
                // Left-drag scrolls the wall; in the lightbox it does nothing (use middle/right to
                // pan), and a left *click* on empty space closes — handled in pointer_up.
                if self.focus.is_none() {
                    let vpw = self.viewport_h() * (self.config.width.max(1) as f32 / h);
                    let world = dx / h * vpw * DRAG_GAIN;
                    self.scroll_x = (self.scroll_x - world).clamp(0.0, max);
                    // velocity (for bank + release fling) is measured from real motion in update().
                }
            }
            DragMode::VideoSeek => {
                // Only move the knob while dragging; the (blocking) seek happens once on release,
                // so scrubbing stays smooth.
                let f = components::video_seek_frac(self.config.width as f32, self.config.height as f32, x);
                self.seek_preview = Some(f);
            }
            DragMode::VideoVol => {
                let f = components::video_vol_frac(self.config.width as f32, self.config.height as f32, x);
                self.set_volume_frac(f);
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
        // End of a scrub: land an exact seek on the final position, then drop the preview.
        if mode == DragMode::VideoSeek {
            if let Some(f) = self.seek_preview.take() {
                self.seek_to_frac(f);
            }
        }
        if mode == DragMode::Scroll && !self.drag_moved {
            if self.focus.is_some() {
                // A click on the playing media toggles pause (video is full-bleed; audio's cover
                // is the lightbox image). A click on the empty/dim area closes — clicking a still
                // image does nothing (so you can't accidentally close while interacting with it).
                let on_media =
                    self.focused_is_video() || self.click_on_lightbox_image(self.drag_last_x, self.drag_last_y);
                if self.video.is_some() && on_media {
                    self.video_command(&["cycle", "pause"]);
                } else if !on_media {
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
        // Center the thumb under the cursor; ease toward it (smooth, like the arrows) rather than
        // snapping — update() animates scroll_x → this target and derives the lean.
        let frac = ((x - pad - thumb_w * 0.5) / travel).clamp(0.0, 1.0);
        self.scroll_target = Some(frac * self.scroll_max.max(0.0));
    }

    fn viewport_h(&self) -> f32 {
        2.0 * (FOV_Y * 0.5).tan() * self.cam_dist
    }

    /// Screen-space NDC rect for the focused video — full-bleed (the mpv surface already matches
    /// the display aspect, letterboxing the clip and placing subtitles), with wheel zoom + pan.
    fn video_rect_ndc(&self) -> [f32; 4] {
        let z = self.lb_zoom;
        [-z + self.lb_pan[0], -z + self.lb_pan[1], 2.0 * z, 2.0 * z]
    }

    /// Pixel layout of the video controls bar.
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
        self.track_menu.is_some() || paused || self.last_activity.elapsed().as_secs_f32() < 2.5
    }

    /// Whether the lightbox controls (close/info/arrows) should show. A focused video hides them
    /// with its controls bar when idle; a focused photo/audio always shows them.
    fn lightbox_controls_visible(&self) -> bool {
        !self.focused_is_video() || self.video_controls_visible()
    }

    pub fn take_fullscreen_request(&mut self) -> bool {
        std::mem::take(&mut self.fullscreen_requested)
    }

    pub fn toggle_info(&mut self) {
        self.show_info = !self.show_info;
    }

    fn apply_sort(&mut self) {
        // Sort is applied when building the view (so "Default" can restore the scan order).
        self.rebuild_view();
    }

    /// Snapshot the data the UI needs this frame.
    fn ui_ctx(&self) -> UiCtx {
        let ready = self
            .resident
            .values()
            .filter(|t| matches!(t, Tile::Ready { .. }))
            .count();
        // Bottom-of-lightbox info pill — only when Info is toggled on: title · full path ·
        // "N / total · When date" (title, line2, line3).
        let info = if self.show_info {
            self.focus.and_then(|i| {
                let p = match self.sources.get(i)? {
                    Source::File(p) | Source::Video(p) | Source::Audio(p) => p,
                    Source::Placeholder(_) => return None,
                };
                let filename =
                    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let title = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| filename.clone());
                // Full path, left-ellipsised so the end (filename) always stays visible.
                let full = p.to_string_lossy();
                let n = full.chars().count();
                let path = if n > 72 {
                    format!("\u{2026}{}", full.chars().skip(n - 71).collect::<String>())
                } else {
                    full.into_owned()
                };
                let pos = format!("{} / {}", i + 1, self.total);
                let use_created = self.date_created
                    || matches!(self.sort_mode, SortMode::CreatedNew | SortMode::CreatedOld);
                let md = std::fs::metadata(p).ok();
                let (when, time) = if use_created {
                    ("Created", md.and_then(|m| m.created().or_else(|_| m.modified()).ok()))
                } else {
                    ("Modified", md.and_then(|m| m.modified().ok()))
                };
                let line3 = match time {
                    Some(t) => format!("{pos}  ·  {when} {}", fmt_date(t)),
                    None => pos,
                };
                Some((title, path, line3))
            })
        } else {
            None
        };
        // Hide the controls bar while the clip is still opening — otherwise the play/pause button
        // shows a "resume" (▶) icon during load. Only the "Loading…" card shows until it's ready.
        let video = if self.video.is_some() && !self.media_starting() {
            let (pos, dur, paused) = self.video_state().unwrap_or((0.0, 0.0, false));
            let (vol, aid, sid) = self
                .video
                .as_ref()
                .map(|v| (v.volume(), v.aid(), v.sid()))
                .unwrap_or((100.0, 0, 0));
            // Only query mpv's track list while a track menu is open (it's an FFI round-trip).
            let (audio_tracks, sub_tracks) = if self.track_menu.is_some() {
                let mut a = Vec::new();
                let mut s = Vec::new();
                if let Some(v) = &self.video {
                    for t in v.tracks() {
                        if t.audio {
                            a.push((t.id, t.label, t.selected));
                        } else {
                            s.push((t.id, t.label, t.selected));
                        }
                    }
                }
                (a, s)
            } else {
                (Vec::new(), Vec::new())
            };
            Some(VideoCtx {
                pos,
                dur,
                paused,
                vol,
                aid,
                sid,
                visible: self.video_controls_visible(),
                scrub: self.seek_preview,
                track_menu: self.track_menu,
                audio_tracks,
                sub_tracks,
            })
        } else {
            None
        };
        UiCtx {
            w: self.config.width as f32,
            h: self.config.height as f32,
            sort: self.sort_mode,
            filter: self.filter_kind,
            menu: self.open_menu,
            search: self.search.clone(),
            search_active: self.search_active,
            slideshow: self.slideshow,
            gif_anim: self.gif_anim,
            reflections: self.reflections,
            date_created: self.date_created,
            date_from: self.date_from.clone(),
            date_to: self.date_to.clone(),
            date_active: self.date_active,
            caret: self.caret,
            show_titles: self.show_titles,
            show_mem: self.show_mem,
            mem_mb: if self.show_mem { process_rss_mb() } else { None },
            show_info: self.show_info,
            total: self.total,
            ready,
            inflight: self.inflight,
            focused: self.focus.is_some(),
            info,
            info_w: [0.0; 3], // measured in render() where &mut ui is available
            video,
            pointer: [
                (self.pointer_ndc[0] + 1.0) * 0.5 * self.config.width as f32,
                (1.0 - self.pointer_ndc[1]) * 0.5 * self.config.height as f32,
            ],
            fullscreen: self.window.fullscreen().is_some(),
            // Blink the text caret ~every 530ms (solid right after activity, then blinking).
            caret_on: self.last_activity.elapsed().as_secs_f32() % 1.06 < 0.53,
        }
    }

    /// Apply a click on the UI (returned by components::hit_test).
    fn apply_ui_action(&mut self, a: UiAction) {
        match a {
            UiAction::OpenFiles => {
                self.open_menu = None;
                self.open_request = Some(OpenKind::Files);
            }
            UiAction::OpenFolder => {
                self.open_menu = None;
                self.open_request = Some(OpenKind::Folder);
            }
            UiAction::OpenJson => {
                self.open_menu = None;
                self.open_request = Some(OpenKind::Json);
            }
            UiAction::Back => self.back(),
            UiAction::Fullscreen => self.fullscreen_requested = true,
            UiAction::ToggleSlideshow => {
                self.slideshow = !self.slideshow;
                self.slideshow_t = Instant::now();
                self.open_menu = None;
                if self.slideshow && self.focus.is_none() && self.total > 0 {
                    self.focus = Some(0); // start the show on the first item
                    self.reset_lb_view();
                }
            }
            UiAction::ToggleInfo => self.show_info = !self.show_info,
            UiAction::ToggleMenu(m) => {
                self.open_menu = if self.open_menu == Some(m) { None } else { Some(m) };
                self.search_active = false;
                self.date_active = 0;
            }
            UiAction::CloseMenu => {
                self.open_menu = None;
                self.date_active = 0;
            }
            UiAction::SetSort(m) => {
                self.open_menu = None;
                if self.sort_mode != m {
                    self.sort_mode = m;
                    self.apply_sort();
                }
            }
            UiAction::SetFilter(f) => {
                self.open_menu = None;
                if self.filter_kind != f {
                    self.filter_kind = f;
                    self.view_dirty = true;
                }
            }
            UiAction::Noop => {}
            UiAction::ToggleGifAnim => self.gif_anim = !self.gif_anim,
            UiAction::ToggleReflections => self.reflections = !self.reflections,
            UiAction::ToggleShowTitles => self.show_titles = !self.show_titles,
            UiAction::ToggleMem => {
                self.show_mem = !self.show_mem;
                if self.show_mem {
                    self.log_memory();
                }
            }
            UiAction::ActivateSearch => {
                self.search_active = true;
                self.date_active = 0;
                self.open_menu = None;
                let x0 = components::search_text_x0(self.config.width as f32);
                self.caret = caret_from_x(self.drag_last_x, x0, 7.3, self.search.chars().count());
            }
            UiAction::ClearSearch => {
                self.search.clear();
                self.view_dirty = true;
            }
            UiAction::SetDateBy(created) => {
                self.date_created = created;
                self.view_dirty = true;
            }
            UiAction::ActivateDate(which) => {
                self.date_active = which;
                self.search_active = false;
                let len = if which == 1 {
                    self.date_from.chars().count()
                } else {
                    self.date_to.chars().count()
                };
                let x0 = components::date_text_x0(self.config.width as f32, which);
                self.caret = caret_from_x(self.drag_last_x, x0, 7.0, len);
            }
            UiAction::ClearDates => {
                self.date_from.clear();
                self.date_to.clear();
                self.date_active = 0;
                self.view_dirty = true;
            }
            UiAction::VideoPause => self.video_command(&["cycle", "pause"]),
            UiAction::VideoSeekRel(s) => self.video_command(&["seek", &s.to_string()]),
            UiAction::VideoSeekFrac(f) => self.seek_to_frac(f),
            UiAction::VideoVolume(vol) => {
                self.video_command(&["set", "volume", &format!("{vol:.0}")])
            }
            UiAction::ToggleTrackMenu(m) => {
                self.track_menu = if self.track_menu == Some(m) { None } else { Some(m) };
            }
            UiAction::SetAudio(id) => {
                if id > 0 {
                    self.video_command(&["set", "aid", &id.to_string()]);
                } else {
                    self.video_command(&["set", "aid", "no"]);
                }
                self.track_menu = None;
            }
            UiAction::SetSub(id) => {
                if id > 0 {
                    self.video_command(&["set", "sid", &id.to_string()]);
                } else {
                    self.video_command(&["set", "sid", "no"]);
                }
                self.track_menu = None;
            }
        }
    }

    /// Any text field (search box or a Dates field) has focus → the keyboard edits it.
    pub fn input_active(&self) -> bool {
        self.search_active || self.date_active != 0
    }

    /// Char count of the focused field (for caret clamping).
    fn active_len(&self) -> usize {
        if self.search_active {
            self.search.chars().count()
        } else if self.date_active == 1 {
            self.date_from.chars().count()
        } else if self.date_active == 2 {
            self.date_to.chars().count()
        } else {
            0
        }
    }

    /// Insert typed text at the caret (Dates fields take only digits/dashes, capped at 10).
    pub fn input_char(&mut self, ch: &str) {
        let c = self.caret;
        let (new_caret, edited) = if self.search_active {
            (insert_into(&mut self.search, c, ch, false), true)
        } else if self.date_active == 1 {
            (insert_into(&mut self.date_from, c, ch, true), true)
        } else if self.date_active == 2 {
            (insert_into(&mut self.date_to, c, ch, true), true)
        } else {
            (c, false)
        };
        if edited {
            self.caret = new_caret;
            self.view_dirty = true;
            self.last_activity = Instant::now(); // keep the caret solid while typing, then blink
        }
    }

    /// Delete the char before the caret (Backspace).
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let i = self.caret - 1;
        let edited = if self.search_active {
            remove_at(&mut self.search, i)
        } else if self.date_active == 1 {
            remove_at(&mut self.date_from, i)
        } else if self.date_active == 2 {
            remove_at(&mut self.date_to, i)
        } else {
            false
        };
        if edited {
            self.caret -= 1;
            self.view_dirty = true;
        }
    }

    /// Delete the char at the caret (Delete).
    pub fn delete_forward(&mut self) {
        let i = self.caret;
        let edited = if self.search_active {
            remove_at(&mut self.search, i)
        } else if self.date_active == 1 {
            remove_at(&mut self.date_from, i)
        } else if self.date_active == 2 {
            remove_at(&mut self.date_to, i)
        } else {
            false
        };
        if edited {
            self.view_dirty = true;
        }
    }

    pub fn caret_left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }
    pub fn caret_right(&mut self) {
        self.caret = (self.caret + 1).min(self.active_len());
    }
    pub fn caret_home(&mut self) {
        self.caret = 0;
    }
    pub fn caret_end(&mut self) {
        self.caret = self.active_len();
    }
    /// Leave the focused field (Enter / Escape).
    pub fn input_done(&mut self) {
        self.search_active = false;
        self.date_active = 0;
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

    /// SVG icons drawn over the wall/lightbox (the prev/next chevrons, centred in the arrow buttons).
    /// The video-control and Back/Info icons are emitted by `components::build`.
    fn icon_reqs(&self) -> Vec<crate::icons::IconReq> {
        let mut v = Vec::new();
        let (prev, next) = self.arrow_rects();
        let tint = [235, 235, 240, 235];
        let centre = |r: [f32; 4], name: &'static str| {
            let s = 32.0_f32;
            crate::icons::IconReq {
                rect: [r[0] + (r[2] - s) * 0.5, r[1] + (r[3] - s) * 0.5, s, s],
                name,
                tint,
            }
        };
        if let Some(f) = self.focus {
            if self.lightbox_controls_visible() {
                if f > 0 {
                    v.push(centre(prev, "prev"));
                }
                if f + 1 < self.total {
                    v.push(centre(next, "next"));
                }
            }
        } else if self.scroll_max > 0.0 {
            v.push(centre(prev, "prev"));
            v.push(centre(next, "next"));
        }
        v
    }

    /// Arrow keys: -1 left, +1 right, 0 released.
    pub fn set_dir(&mut self, dir: f32) {
        self.input_dir = dir;
        if dir != 0.0 {
            self.scroll_target = None; // arrows take over from an eased scrub
        }
    }

    pub fn update(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;

        // Search/filter changed → rebuild the displayed view.
        if self.view_dirty {
            self.view_dirty = false;
            self.rebuild_view();
        }

        // Slideshow: auto-advance the focused item every few seconds (wraps; stops if closed).
        if self.slideshow {
            if self.focus.is_none() || self.total == 0 {
                self.slideshow = false;
            } else if self.slideshow_t.elapsed().as_secs_f32() >= 4.0 {
                self.slideshow_t = Instant::now();
                self.focus = Some(self.focus.map_or(0, |f| (f + 1) % self.total));
                self.reset_lb_view();
            }
        }

        // --- scroll physics (frozen while a tile is focused) ---
        // A live pointer drag owns scroll_x directly (set in pointer_move / scrub_to). Integrating
        // inertia on top would double-move it — and, worse, keep it drifting and banked while the
        // cursor holds still. So while dragging we only *measure* the actual per-frame speed (low-
        // passed) to drive the bank; that measured speed then becomes the fling when you let go.
        if self.focus.is_none() {
            let max = self.scroll_max.max(0.0);
            if let Some(t) = self.scroll_target {
                // Eased scrub/slider target → smooth motion (like the arrows) with a derived lean.
                let old = self.scroll_x;
                self.scroll_x = (self.scroll_x + (t - self.scroll_x) * (14.0 * dt).min(1.0)).clamp(0.0, max);
                self.velocity = ((self.scroll_x - old) / dt.max(1e-4)).clamp(-MAX_SPEED, MAX_SPEED);
                if self.drag_mode != DragMode::Scrub && (t - self.scroll_x).abs() < 0.004 {
                    self.scroll_x = t.clamp(0.0, max);
                    self.scroll_target = None;
                    self.velocity = 0.0;
                }
            } else {
                match self.drag_mode {
                    // Left-drag moves scroll_x directly; measure the real speed so the wall leans
                    // into the motion (and flings on release).
                    DragMode::Scroll => {
                        let measured = (self.scroll_x - self.prev_scroll_x) / dt.max(1e-4);
                        self.velocity += (measured - self.velocity) * 0.35; // low-pass → stable bank
                        self.velocity = self.velocity.clamp(-MAX_SPEED, MAX_SPEED);
                    }
                    // Middle/right grab-pan is a flat 2D move — no lean, no fling.
                    DragMode::Pan => self.velocity = 0.0,
                    DragMode::Scrub | DragMode::VideoSeek | DragMode::VideoVol => {
                        self.velocity = 0.0
                    }
                    DragMode::None => {
                        if self.input_dir != 0.0 {
                            self.velocity = (self.velocity + self.input_dir * ACCEL * dt)
                                .clamp(-MAX_SPEED, MAX_SPEED);
                        } else {
                            self.velocity *= 0.045_f32.powf(dt); // momentum decay (eases out slowly)
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

        // No hover while an item is focused (otherwise the name tooltip lingers over the lightbox).
        if self.focus.is_some() {
            self.hover_index = None;
        }
        // Ease each tile's hover zoom independently so moving between tiles is smooth (the one
        // under the cursor grows toward 1.12, the rest shrink back to 1.0 and are then dropped).
        let hovered = if self.focus.is_none() {
            self.hover_index
        } else {
            None
        };
        if let Some(i) = hovered {
            self.hover_scales.entry(i).or_insert(1.0);
        }
        let k = (10.0 * dt).min(1.0);
        self.hover_scales.retain(|&idx, sc| {
            let target = if Some(idx) == hovered { 1.12 } else { 1.0 };
            *sc += (target - *sc) * k;
            // keep while still animating or actively hovered
            Some(idx) == hovered || (*sc - 1.0).abs() > 0.004
        });

        // --- focus in/out transition ---
        // Video/audio open instantly (they sit on solid black — nothing to animate); photos keep the
        // eased zoom/dim. Closing always eases back.
        let opening_media = matches!(
            self.focus.and_then(|i| self.sources.get(i)),
            Some(Source::Video(_) | Source::Audio(_))
        );
        let target_t = if self.focus.is_some() { 1.0 } else { 0.0 };
        if opening_media {
            self.focus_t = 1.0;
        } else {
            let step = ANIM_SPEED * dt;
            self.focus_t = if self.focus_t < target_t {
                (self.focus_t + step).min(target_t)
            } else {
                (self.focus_t - step).max(target_t)
            };
        }

        // --- camera zoom smoothing (wheel target, or pull-in when focused) ---
        let target_dist = if self.focus.is_some() {
            FOCUS_DIST
        } else {
            self.cam_dist_target
        };
        self.cam_dist += (target_dist - self.cam_dist) * (6.0 * dt).min(1.0);

        // Zoom toward the cursor: as the viewport shrinks/grows with a *wheel* zoom, shift the wall
        // so the point under the pointer stays put (matches the web). Skipped during the focus
        // open/close transition (focus_t animating) — otherwise the zoom-out on close drifts the
        // wall toward the cursor and you don't land centered on the item you were viewing.
        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let vph = self.viewport_h();
        let vpw = vph * aspect;
        if self.focus.is_none() && self.focus_t < 0.01 && self.last_vp[0] > 0.0 {
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
            Some(f) => {
                // Decode a crisp full-res image for the lightbox: photos, the video poster (clean,
                // no badge), and the audio cover art. Skip animated GIFs — their frames own full_tex.
                let want = match self.sources.get(f) {
                    Some(Source::File(p)) => !(self.gif_anim && is_gif(p)),
                    Some(Source::Audio(_)) => true, // crisp cover art (videos draw via their own quad)
                    Some(Source::Placeholder(_)) => true, // sample (SVG) tiles open in the lightbox too
                    _ => false,
                };
                if want && self.full_pending != Some(f) && self.full_for != Some(f) {
                    self.full_pending = Some(f);
                    // Priority lane → decodes ahead of wall-thumb streaming, so the open is crisp fast.
                    let _ = self.pjob_tx.send(Job {
                        index: f,
                        source: self.sources[f].clone(),
                        gen: self.generation,
                        full: true,
                    });
                } else if !want {
                    self.full_pending = None;
                    self.full_for = None;
                }
            }
            None => {
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

        self.upload_camera();

        // --- focused-video playback: start mpv when a video tile is focused, stop on change ---
        if self.focus != self.video_for {
            self.video = None; // dropping the player stops mpv
            self.video_for = self.focus;
            self.track_menu = None; // close any track menu from the previous item
            // Defer the mpv start one frame so the poster thumbnail paints first — mpv's init blocks
            // the thread ~100ms+, which otherwise shows as a hitch the instant you open a clip.
            self.video_pending = self
                .focus
                .filter(|&i| matches!(self.sources.get(i), Some(Source::Video(_) | Source::Audio(_))));
        } else if let Some(idx) = self.video_pending.take() {
            if let Source::Video(path) | Source::Audio(path) = self.sources[idx].clone() {
                self.video = Some(crate::video::Player::start(
                    &self.device,
                    &self.queue,
                    self.config.format,
                    &path,
                ));
                // Carry the last-set volume to the new clip (a new mpv starts at 100).
                self.video_command(&["set", "volume", &format!("{:.0}", self.volume)]);
            }
        }
        if let Some(v) = &mut self.video {
            v.update(&self.device, &self.queue, self.config.width, self.config.height);
        }
        // Remember the current volume so the next clip opens at the same level.
        if let Some(v) = &self.video {
            self.volume = v.volume();
        }

        // --- focused GIF animation (Settings → Animate GIFs): decode frames off-thread, then
        // cycle them into full_tex (which the lightbox samples) on their per-frame delays ---
        // A focused (opened) GIF always animates. The "Animate GIFs" setting governs only the
        // wall thumbnails, like the web wall.
        let gif_target = self
            .focus
            .filter(|&i| matches!(self.sources.get(i), Some(Source::File(p)) if is_gif(p)));
        match gif_target {
            Some(i) => {
                let have = self.gif.as_ref().map(|g| g.index) == Some(i);
                if !have && self.gif_pending != Some(i) {
                    if let Some(Source::File(p)) = self.sources.get(i).cloned() {
                        self.gif = None;
                        self.gif_pending = Some(i);
                        let (tx, gen) = (self.gif_tx.clone(), self.generation);
                        std::thread::spawn(move || {
                            let frames = decode_gif(&p, FULL_PX, 400);
                            let _ = tx.send(GifMsg { index: i, gen, frames });
                        });
                    }
                }
            }
            None => {
                self.gif = None;
                self.gif_pending = None;
            }
        }
        while let Ok(msg) = self.gif_rx.try_recv() {
            if self.gif_pending == Some(msg.index) {
                self.gif_pending = None;
            }
            if msg.gen == self.generation && self.focus == Some(msg.index) && !msg.frames.is_empty() {
                self.gif = Some(GifAnim {
                    index: msg.index,
                    frames: msg.frames,
                    cur: 0,
                    t: 0.0,
                });
                self.upload_gif_frame(0);
            }
        }
        let mut advance = None;
        if let Some(g) = self.gif.as_mut() {
            if self.focus == Some(g.index) && g.frames.len() > 1 {
                g.t += dt;
                while g.t >= g.frames[g.cur].delay {
                    g.t -= g.frames[g.cur].delay;
                    g.cur = (g.cur + 1) % g.frames.len();
                }
                advance = Some(g.cur);
            }
        }
        if let Some(c) = advance {
            self.upload_gif_frame(c);
        }

        // --- wall GIF thumbnails (Settings → Animate GIFs): each GIF is decoded once into a packed
        // atlas in its own tile layer, then animated purely by a UV-offset change — no per-frame
        // uploads and ~no extra memory. Sampled at one low global tick, time-correct, like the web.
        self.wall_gifs.retain(|i, _| self.resident.contains_key(i)); // drop evicted tiles
        if self.gif_anim && self.focus.is_none() && self.velocity.abs() <= GIF_FAST_VEL {
            // Resident GIF tiles without an atlas yet, nearest the view centre first.
            let center = self.view_center();
            let mut cands: Vec<usize> = self
                .resident
                .iter()
                .filter(|(&i, t)| {
                    matches!(t, Tile::Ready { .. })
                        && !self.wall_gifs.contains_key(&i)
                        && !self.wall_gif_pending.contains(&i)
                        && matches!(self.sources.get(i), Some(Source::File(p)) if is_gif(p))
                })
                .map(|(&i, _)| i)
                .collect();
            cands.sort_by_key(|&i| ((i / ROWS) as i64 - center).abs());
            for i in cands {
                if self.wall_gif_pending.len() >= MAX_GIF_DECODES {
                    break;
                }
                if let Some(Source::File(p)) = self.sources.get(i).cloned() {
                    self.wall_gif_pending.insert(i);
                    let (tx, gen) = (self.wall_gif_tx.clone(), self.generation);
                    std::thread::spawn(move || {
                        let atlas = decode_gif_atlas(&p);
                        let _ = tx.send(WallGifMsg { index: i, gen, atlas });
                    });
                }
            }
        }
        while let Ok(msg) = self.wall_gif_rx.try_recv() {
            self.wall_gif_pending.remove(&msg.index);
            if let Some(a) = msg.atlas {
                if msg.gen == self.generation {
                    if let Some(&Tile::Ready { layer, .. }) = self.resident.get(&msg.index) {
                        // Upload the packed grid into the tile's layer once.
                        self.upload_layer(layer, &a.rgba, 512, 512);
                        self.wall_gifs.insert(
                            msg.index,
                            GifAtlasAnim {
                                grid: a.grid,
                                cell: a.cell,
                                fw: a.fw,
                                fh: a.fh,
                                delays: a.delays,
                                total: a.total,
                                start: Instant::now(),
                                cur: 0,
                            },
                        );
                    }
                }
            }
        }
        // Single low global tick (web wall's ~4fps): pick each GIF's time-correct frame. This only
        // moves a cursor — rebuild_instances samples the matching atlas cell, no uploads.
        if self.gif_anim && self.gif_tick.elapsed().as_secs_f32() >= GIF_WALL_TICK_S {
            self.gif_tick = Instant::now();
            for g in self.wall_gifs.values_mut() {
                if g.delays.len() <= 1 {
                    continue;
                }
                let elapsed = g.start.elapsed().as_secs_f32() % g.total;
                let mut acc = 0.0;
                g.cur = g.delays.len() - 1;
                for (i, &d) in g.delays.iter().enumerate() {
                    acc += d;
                    if elapsed < acc {
                        g.cur = i;
                        break;
                    }
                }
            }
        }

        // Rebuild wall instances last — after the GIF atlases upload and the frame cursor advances —
        // so each animated tile samples a single (current) cell this frame, never the packed grid.
        self.rebuild_instances();

        // A decode burst just finished — hand the freed decode buffers back to the OS.
        if self.prev_inflight > 0 && self.inflight == 0 {
            trim_heap();
        }
        self.prev_inflight = self.inflight;

        // Memory-usage log (Settings → Memory usage): a detailed snapshot to a file every ~2s.
        if self.show_mem && self.mem_log.elapsed().as_secs_f32() >= 2.0 {
            self.mem_log = Instant::now();
            self.log_memory();
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

    /// Upload GIF frame `c` into full_tex (the lightbox samples it), so the focused GIF animates.
    fn upload_gif_frame(&mut self, c: usize) {
        let Some(g) = self.gif.as_ref() else {
            return;
        };
        let Some(f) = g.frames.get(c) else {
            return;
        };
        let (w, h, idx) = (f.w, f.h, g.index);
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.full_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &f.rgba,
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
        self.full_extent = [w as f32 / FULL_PX as f32, h as f32 / FULL_PX as f32];
        self.full_for = Some(idx);
    }

    fn rebuild_instances(&mut self) {
        // Draw every tile in the visible window: a dark placeholder "skeleton" at the default size
        // until the image decodes, then the image at its true aspect. This keeps the grid full and
        // evenly spaced (matching the web) instead of leaving holes where tiles haven't loaded.
        // Reflections first so they paint behind the photos (no depth buffer → paint order).
        let mut refl: Vec<Instance> = Vec::new();
        let mut placeholders: Vec<Instance> = Vec::new();
        let mut photos: Vec<Instance> = Vec::new();
        let mut hovered: Option<Instance> = None; // drawn last so it sits above its neighbours
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
                    // Hover zoom-in-place (scales about the tile's center); the actively hovered
                    // tile draws last so it sits above its neighbours.
                    let sc = self.hover_scales.get(&i).copied().unwrap_or(1.0);
                    // Animated GIF: sample its current atlas cell; otherwise the whole image.
                    let (uv_off, uv_ext) = match self.wall_gifs.get(&i) {
                        Some(a) if a.grid > 0 => {
                            let cw = a.cell as f32 / TILE_PX as f32;
                            let (gx, gy) = (a.cur % a.grid as usize, a.cur / a.grid as usize);
                            (
                                [gx as f32 * cw, gy as f32 * cw],
                                [a.fw as f32 / TILE_PX as f32, a.fh as f32 / TILE_PX as f32],
                            )
                        }
                        _ => ([0.0, 0.0], *uv),
                    };
                    let inst = Instance {
                        offset: [cx, baseline + h * 0.5],
                        size: [w * sc, h * sc],
                        layer: *layer,
                        uv_extent: uv_ext,
                        kind: 0,
                        uv_offset: uv_off,
                    };
                    if self.hover_index == Some(i) {
                        hovered = Some(inst);
                    } else {
                        photos.push(inst);
                    }
                    // The bottom row sits on glass: a mirrored, fading copy hangs beneath it.
                    if row == ROWS - 1 && self.reflections {
                        refl.push(Instance {
                            offset: [cx, baseline - REFLECT_GAP - h * 0.5],
                            size: [w, h],
                            layer: *layer,
                            uv_extent: uv_ext,
                            kind: 1,
                            uv_offset: uv_off,
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
                        uv_offset: [0.0, 0.0],
                    });
                }
            }
        }
        // Paint order: reflections (behind), then skeletons, then photos, then the hovered tile.
        refl.extend(placeholders);
        refl.extend(photos);
        refl.extend(hovered);
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

    /// The current camera view-projection (perspective · view · bank), blended toward the focused
    /// tile. Shared by the GPU upload and by show-titles screen projection.
    fn view_proj_matrix(&self) -> Mat4 {
        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let proj = Mat4::perspective_rh(FOV_Y, aspect, 0.1, 100.0);
        let s = smoothstep(self.focus_t);
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
        proj * view * model
    }

    fn upload_camera(&self) {
        let u = CameraUniform {
            view_proj: self.view_proj_matrix().to_cols_array_2d(),
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

    /// True when the focused item is a video (full-bleed playback). Audio also plays via mpv but
    /// shows its cover art in the photo lightbox, so it's not "a video" for input purposes.
    pub fn focused_is_video(&self) -> bool {
        matches!(self.focus.and_then(|i| self.sources.get(i)), Some(Source::Video(_)))
    }

    /// Forward an mpv command to the playing video (no-op when nothing is playing or the `video`
    /// feature is off). e.g. ["cycle","pause"], ["cycle","aid"], ["cycle","sid"].
    pub fn video_command(&self, args: &[&str]) {
        if let Some(v) = &self.video {
            v.command(args);
        }
    }

    /// Seek the playing video to a fraction (0..1) of its duration (exact).
    fn seek_to_frac(&self, f: f32) {
        if let (Some(v), Some((_, dur, _))) = (self.video.as_ref(), self.video_state()) {
            if dur > 0.0 {
                v.seek(f as f64 * dur);
            }
        }
    }

    /// Set the playing video/audio volume from a fraction (0..1).
    fn set_volume_frac(&self, f: f32) {
        let vol = (f * 100.0).clamp(0.0, 100.0);
        self.video_command(&["set", "volume", &format!("{vol:.0}")]);
    }

    /// Prev/next item in the lightbox (dir = -1 / +1).
    pub fn navigate(&mut self, dir: i64) {
        if let (Some(f), true) = (self.focus, self.total > 0) {
            self.focus = Some((f as i64 + dir).clamp(0, self.total as i64 - 1) as usize);
            self.reset_lb_view();
        }
    }

    /// True while a focused video/audio is opening (deferred mpv start, or mpv still loading) —
    /// so we show a "Loading…" card instead of a black/blank centre. A video loads until its first
    /// frame; audio until mpv reports a duration.
    fn media_starting(&self) -> bool {
        let Some(i) = self.focus else { return false };
        let is_video = matches!(self.sources.get(i), Some(Source::Video(_)));
        let is_audio = matches!(self.sources.get(i), Some(Source::Audio(_)));
        if !is_video && !is_audio {
            return false;
        }
        if self.video_pending.is_some() {
            return true; // start deferred a frame
        }
        match &self.video {
            Some(v) if is_video => !v.has_frame(),
            // Audio: stay loading until mpv has a duration AND the crisp cover art is decoded, so it
            // opens to the cover on black (like a video opens to its frame) — not over the laggy wall.
            Some(_) => {
                let dur_ok = self.video_state().map(|(_, dur, _)| dur > 0.0).unwrap_or(false);
                !dur_ok || self.full_for != self.focus
            }
            None => true, // focused but the player isn't up yet
        }
    }


    /// Append a detailed memory snapshot to <temp>/cooliris-memory.log (Settings → Memory usage).
    /// Kept OUT of the terminal — it's for diagnosing what holds memory, not user-facing noise.
    fn log_memory(&self) {
        let Some(mb) = process_rss_mb() else { return };
        let ready = self
            .resident
            .values()
            .filter(|t| matches!(t, Tile::Ready { .. }))
            .count();
        let gif_frames = self.gif.as_ref().map(|g| g.frames.len()).unwrap_or(0);
        // Wall GIFs are packed into their tile layers (no extra RAM); report count + total frames.
        let wall_gif_frames: usize = self.wall_gifs.values().map(|g| g.delays.len()).sum();
        let wall_gif_mb = 0; // atlas lives in the pool layer, not the heap
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "{secs} | RSS {mb} MB | resident {} (ready {}) | free_layers {}/{} | inflight {} | \
             full_for {:?} gif_frames {} video {} | wall_gifs {} ({wall_gif_frames}f, {wall_gif_mb}MB) | total {}\n",
            self.resident.len(),
            ready,
            self.free_layers.len(),
            POOL,
            self.inflight,
            self.full_for,
            gif_frames,
            self.video.is_some(),
            self.wall_gifs.len(),
            self.total,
        );
        let path = std::env::temp_dir().join("cooliris-memory.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    pub fn current_folder(&self) -> Option<&std::path::Path> {
        self.current_folder.as_deref()
    }

    /// Swap the library to a pre-scanned set of sources (scanning happens off the main thread so
    /// a big/slow folder doesn't freeze the window). The generation bump drops in-flight decodes
    /// from the old library; the texture pool, pipelines and worker threads are all reused.
    pub fn reload_with(&mut self, folder: Option<PathBuf>, sources: Vec<Source>) {
        self.all_sources = Arc::new(sources); // original order; rebuild_view sorts the view
        self.current_folder = folder;
        self.scanning = false;
        self.rebuild_view();
        log::info!("loaded {} tiles", self.total);
    }

    /// Rebuild the displayed wall (`sources`) from the full library, applying the type filter and
    /// the search query, then reset the streaming state. Used by load, sort, filter and search.
    fn rebuild_view(&mut self) {
        let q = self.search.to_lowercase();
        let kind = self.filter_kind;
        // Date range (Dates filter): only enforced once a bound parses to a full date.
        let from = parse_ymd_days(&self.date_from);
        let to = parse_ymd_days(&self.date_to);
        let date_created = self.date_created;
        let name_of = |s: &Source| match s {
            Source::File(p) | Source::Video(p) | Source::Audio(p) => {
                p.file_name().map(|n| n.to_string_lossy().to_lowercase())
            }
            Source::Placeholder(_) => None,
        };
        fn path_of(s: &Source) -> Option<&std::path::Path> {
            match s {
                Source::File(p) | Source::Video(p) | Source::Audio(p) => Some(p.as_path()),
                Source::Placeholder(_) => None,
            }
        }
        let filtered: Vec<Source> = self
            .all_sources
            .iter()
            .filter(|s| {
                let kind_ok = match s {
                    Source::Placeholder(_) => true,
                    Source::File(_) => matches!(kind, Filter::All | Filter::Photos),
                    Source::Video(_) => matches!(kind, Filter::All | Filter::Videos),
                    Source::Audio(_) => matches!(kind, Filter::All | Filter::Audio),
                };
                let search_ok = q.is_empty() || name_of(s).map(|n| n.contains(&q)).unwrap_or(true);
                let date_ok = if from.is_none() && to.is_none() {
                    true
                } else {
                    match path_of(s).and_then(|p| file_days(p, date_created)) {
                        Some(d) => from.map_or(true, |f| d >= f) && to.map_or(true, |t| d <= t),
                        None => path_of(s).is_none(), // placeholders pass; unreadable dates drop
                    }
                };
                kind_ok && search_ok && date_ok
            })
            .cloned()
            .collect();

        self.generation += 1;
        self.sources = Arc::new(sort_sources(filtered, self.sort_mode));
        self.total = self.sources.len();
        self.total_cols = self.total.div_ceil(ROWS) as i64;
        self.scroll_max = (self.total_cols - 1).max(0) as f32 * CELL_X;
        self.resident.clear();
        self.wall_gifs.clear();
        self.wall_gif_pending.clear();
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
    }

    /// Which picker the Open menu requested since the last check (main opens the native dialog).
    pub fn take_open_request(&mut self) -> Option<OpenKind> {
        self.open_request.take()
    }

    /// Close any open top-bar menu/modal (e.g. after a file is dropped on the Open dialog).
    pub fn close_menu(&mut self) {
        self.open_menu = None;
        self.date_active = 0;
    }

    /// Mark that a folder is being picked/scanned (shows a "Scanning folder…" indicator).
    pub fn set_scanning(&mut self, b: bool) {
        self.scanning = b;
        if b {
            self.scan_count = 0;
        }
    }

    /// Update the scan progress (media files found so far), shown while scanning.
    pub fn set_scan_count(&mut self, n: usize) {
        self.scan_count = n;
    }

    /// Show-titles (Settings): for each on-screen wall tile, the title pill anchored to the tile's
    /// bottom-left — returns (name, box_x, box_y, box_w) in pixels (box height is fixed). The black
    /// box is drawn by overlay_rects, the white text by ui_lines, both from this list.
    fn wall_titles(&self) -> Vec<(String, f32, f32, f32)> {
        let mut out = Vec::new();
        if !self.show_titles || self.focus.is_some() || self.scanning || self.open_menu.is_some() {
            return out;
        }
        let vp = self.view_proj_matrix();
        let w = self.config.width as f32;
        let h = self.config.height as f32;
        for (&i, tile) in &self.resident {
            if !matches!(tile, Tile::Ready { .. }) {
                continue;
            }
            let (cx, _) = self.tile_center(i);
            let by = row_baseline(i % ROWS); // tile bottom
            // Project the tile's bottom-centre → screen, then centre the pill on it.
            let p = vp * Vec4::new(cx, by, 0.0, 1.0);
            if p.w <= 0.05 {
                continue;
            }
            let (nx, ny) = (p.x / p.w, p.y / p.w);
            if !(-1.1..=1.1).contains(&nx) || !(-1.1..=1.1).contains(&ny) {
                continue;
            }
            let sx = (nx * 0.5 + 0.5) * w;
            let sy = (1.0 - (ny * 0.5 + 0.5)) * h;
            let Some(name) = (match self.sources.get(i) {
                Some(Source::File(p) | Source::Video(p) | Source::Audio(p)) => {
                    p.file_name().map(|n| n.to_string_lossy().into_owned())
                }
                _ => None,
            }) else {
                continue;
            };
            let name = if name.chars().count() > 22 {
                name.chars().take(21).collect::<String>() + "\u{2026}"
            } else {
                name
            };
            let bw = name.chars().count() as f32 * 7.0 + 16.0;
            // Pill centred on the tile's bottom, overlapping the lower image.
            out.push((name, sx - bw * 0.5, sy - 28.0, bw));
            if out.len() >= 80 {
                break;
            }
        }
        out
    }

    /// Toolbar text: the Open label + a folder hint or the loaded/total readout.
    fn ui_lines(&self) -> Vec<crate::ui::Line> {
        // The top bar / dropdowns / info / video labels are drawn by `components`; this layer only
        // draws wall-space text: centered status, lightbox hints, edge arrows, hover tooltip.
        let mut v: Vec<crate::ui::Line> = Vec::new();
        let cx = self.config.width as f32 * 0.5;
        let cy = self.config.height as f32 * 0.5;
        if self.scanning {
            let text = if self.scan_count > 0 {
                format!("Scanning folder…  {} found", self.scan_count)
            } else {
                "Scanning folder…".into()
            };
            let half = text.chars().count() as f32 * 8.0 * 0.5;
            v.push(crate::ui::Line {
                text,
                x: cx - half,
                y: cy - 20.0,
                size: 30.0,
                color: [235, 235, 240, 255],
            });
        } else if let Some(idx) = self.focus {
            // Lightbox: prev/next chevrons are SVG icons now (see icon_reqs); "Loading…" until decoded.
            // (Position / total now lives in the Info card; no bottom hint here.)
            if matches!(self.sources.get(idx), Some(Source::Video(_))) && !cfg!(feature = "video") {
                // This build has no libmpv linked — explain why the clip isn't playing.
                v.push(crate::ui::Line {
                    text: "▶  video — rebuild with  --features video  to play".into(),
                    x: cx - 230.0,
                    y: cy - 16.0,
                    size: 22.0,
                    color: [235, 235, 240, 255],
                });
            }
            // (No "Loading…" card — video/audio open onto plain black and the content swaps in when
            // ready; video controls are drawn by `components`.)
        } else {
            // On-screen left/right scroll arrows (SVG icons now; see icon_reqs).
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
            // Hover tooltip: the item's name, centred in its black pill (same as show-titles).
            if let Some((name, bx, by, _bw)) = self.hover_label() {
                v.push(crate::ui::Line {
                    text: name,
                    x: bx + 8.0,
                    y: by + 5.0,
                    size: 12.0,
                    color: [240, 240, 245, 255],
                });
            }
        }

        // Show-titles (Settings): white text inside each tile's bottom-left pill (boxes are pushed
        // by overlay_rects, from the same wall_titles() list).
        for (name, bx, by, _bw) in self.wall_titles() {
            v.push(crate::ui::Line {
                text: name,
                x: bx + 8.0,
                y: by + 5.0,
                size: 12.0,
                color: [240, 240, 245, 255],
            });
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
        // Videos never use the lightbox image path: the playing frame is drawn by the video quad
        // (only once mpv has a frame), and the centred "Loading…" card covers the spin-up — so no
        // poster/badge flashes before it opens.
        if matches!(self.sources.get(idx), Some(Source::Video(_))) {
            return LbDraw::None;
        }
        // Audio: show nothing (the black Loading screen) until its cover art is ready, then the cover.
        if matches!(self.sources.get(idx), Some(Source::Audio(_))) && self.media_starting() {
            return LbDraw::None;
        }
        // (aspect, uv extent, uv offset, layer, which bind group) — full-res if ready, else the
        // thumb. An animated GIF's tile layer holds a packed atlas (a grid of frames): until the
        // full-res GIF (full_tex) is decoded, sample its *current cell* (uv_off) so the open shows
        // the animating frame, not a black screen or the whole grid.
        let (aspect, uv, uv_off, layer, draw) = if self.full_for == Some(idx) {
            let e = self.full_extent;
            (e[0] / e[1], e, [0.0, 0.0], 0.0, LbDraw::Full)
        } else if let Some(a) = self.wall_gifs.get(&idx) {
            let Some(&Tile::Ready { layer, .. }) = self.resident.get(&idx) else {
                return LbDraw::None;
            };
            let g = a.grid.max(1) as usize;
            let cur = a.cur.min(a.delays.len().saturating_sub(1));
            let cell_uv = a.cell as f32 / 512.0;
            let uv_off = [(cur % g) as f32 * cell_uv, (cur / g) as f32 * cell_uv];
            let uv_ext = [a.fw as f32 / 512.0, a.fh as f32 / 512.0];
            (a.fw as f32 / a.fh.max(1) as f32, uv_ext, uv_off, layer as f32, LbDraw::Thumb)
        } else {
            // Never show the low-res wall thumbnail in the lightbox — wait for the full-res decode
            // (priority lane makes it quick) so an opened photo is always at best quality.
            return LbDraw::None;
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
            uv_off: [uv_off[0], uv_off[1], 0.0, 0.0],
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
            color: [0.0, 0.0, 0.0, 0.93 * s], round: [0.0; 4] });

        // (Top bar, dropdowns, search and video controls are built by `components` and merged in
        // render — this layer only draws the wall overlay below.)

        // Edge arrow button backgrounds. Focused: prev/next (fade in, hidden at the ends). On the
        // wall: left/right scroll buttons (when scrollable).
        let to_ndc = |r: [f32; 4]| [nx(r[0]), ny_top(r[1] + r[3]), nw(r[2]), nhh(r[3])];
        let (prev, next) = self.arrow_rects();
        // A rounded, transparent black "glass" arrow button centred in the arrow's (taller) hit
        // region (lighter when the cursor is over it). No border — borders are panel-only now.
        let (mx, my) = ((self.pointer_ndc[0] + 1.0) * 0.5 * w, (1.0 - self.pointer_ndc[1]) * 0.5 * h);
        let mut arrow_btn = |r: [f32; 4], a: f32| {
            let (bw, bh) = (62.0_f32, 92.0_f32);
            let bx = r[0] + (r[2] - bw) * 0.5;
            let by = r[1] + (r[3] - bh) * 0.5;
            let hov = mx >= r[0] && mx <= r[0] + r[2] && my >= r[1] && my <= r[1] + r[3];
            let fl = if hov { components::ARROW_HOVER } else { components::ARROW_FILL };
            rects.push(OverlayRect {
                rect: to_ndc([bx, by, bw, bh]),
                color: [fl[0], fl[1], fl[2], fl[3] * a], round: [30.0, bw, bh, 0.0] });
        };
        if let Some(f) = self.focus {
            if self.lightbox_controls_visible() {
                if f > 0 {
                    arrow_btn(prev, s);
                }
                if f + 1 < self.total {
                    arrow_btn(next, s);
                }
            }
        } else if self.scroll_max > 0.0 {
            arrow_btn(prev, 1.0);
            arrow_btn(next, 1.0);
        }

        // Bottom scrubber (only when scrollable and not focused): a taller track with evenly spaced
        // tick lines and a draggable thumb whose width shows how much of the library is on screen.
        if self.focus.is_none() && self.scroll_max > 0.0 {
            let (pad, track_w, thumb_w) = self.scrubber_geom();
            let bh = nhh(18.0); // taller bar
            let by = -1.0 + nhh(14.0);
            rects.push(OverlayRect {
                rect: [nx(pad), by, nw(track_w), bh],
                color: [1.0, 1.0, 1.0, 0.14], round: [9.0, track_w, 18.0, 0.0] }); // rounded (pill) ends
            // Evenly-spaced tick lines across the track — denser now (min 24, capped so we never overflow).
            let ticks = (self.total_cols.max(1) as usize).clamp(24, 96);
            if ticks > 1 {
                let tw = nw(1.5);
                let th = nhh(10.0);
                let ty = -1.0 + nhh(18.0);
                // Interior ticks only — skip the first/last so they don't sit on the rounded ends.
                for k in 1..ticks {
                    let fx = k as f32 / ticks as f32;
                    let x = pad + (track_w - 1.5) * fx;
                    rects.push(OverlayRect {
                        rect: [nx(x), ty, tw, th],
                        color: [1.0, 1.0, 1.0, 0.18], round: [0.75, 1.5, 10.0, 0.0] });
                }
            }
            let frac = (self.scroll_x / self.scroll_max).clamp(0.0, 1.0);
            let thumb_x = pad + (track_w - thumb_w) * frac;
            rects.push(OverlayRect {
                rect: [nx(thumb_x), by, nw(thumb_w), bh],
                color: [0.95, 0.96, 1.0, 0.9], round: [9.0, thumb_w, 18.0, 0.0] }); // full-round (pill) ends
        }

        // Hover tooltip: a rounded black pill centred on the hovered tile (text drawn in ui_lines),
        // matching the show-titles style.
        if let Some((_name, bx, by, bw)) = self.hover_label() {
            rects.push(OverlayRect {
                rect: [nx(bx), ny_top(by + 22.0), nw(bw), nhh(22.0)],
                color: [0.0, 0.0, 0.0, 1.0], round: [6.0, bw, 22.0, 0.0] });
        }

        // Show-titles: a rounded black pill at each tile's bottom (text drawn in ui_lines).
        for (_name, bx, by, bw) in self.wall_titles() {
            rects.push(OverlayRect {
                rect: [nx(bx), ny_top(by + 22.0), nw(bw), nhh(22.0)],
                color: [0.0, 0.0, 0.0, 1.0], round: [6.0, bw, 22.0, 0.0] });
        }

        rects
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());

        // Wall overlay (dim, scrubber, arrows, hover bg) + the custom UI (top bar, dropdowns,
        // search, video controls) built by `components`.
        let mut overlay = self.overlay_rects();
        let mut lines = self.ui_lines();
        let mut icons = self.icon_reqs();
        let mut uic = self.ui_ctx();
        // Measure the info-card lines now (needs &mut ui) so they centre exactly.
        if let Some((a, b, c)) = uic.info.clone() {
            uic.info_w = [
                self.ui.text_width(&a, 14.0),
                self.ui.text_width(&b, 12.0),
                self.ui.text_width(&c, 12.0),
            ];
        }
        let (ui_rects, ui_lines, ui_icons) = components::build(&uic);
        overlay.extend(ui_rects);
        lines.extend(ui_lines);
        icons.extend(ui_icons);
        overlay.truncate(OVERLAY_CAP as usize);
        if !overlay.is_empty() {
            self.queue
                .write_buffer(&self.overlay_buf, 0, bytemuck::cast_slice(&overlay));
        }
        let lb_visible = self.prepare_lightbox();
        self.ui.prepare(
            &self.device,
            &self.queue,
            self.config.width,
            self.config.height,
            &lines,
        );

        // Lightbox backdrop: a focused photo dims the wall to a blurred backdrop (mix → smoothstep).
        // A focused video/audio always sits on SOLID BLACK instead — no wall render, no blur, no
        // open animation — so the clip/cover never flashes the wall behind it (and opening is fast).
        let focused_media = matches!(
            self.focus.and_then(|i| self.sources.get(i)),
            Some(Source::Video(_) | Source::Audio(_))
        );
        let mix = if focused_media { 1.0 } else { smoothstep(self.focus_t) };
        self.post.set_params(&self.queue, mix, 0.0);

        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        // Pass 1: the wall (tiles + reflections) → the offscreen scene texture. Skipped whenever a
        // video/audio is focused (the backdrop is solid black then), so the wall isn't re-rendered
        // behind a clip/cover — that also kept the wall from flashing in after loading.
        if !focused_media {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.post.scene_view(),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
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

        // Passes 2–3: blur the scene for the backdrop (only for a focused photo — video/audio sit
        // on plain black, so the blur would be wasted work).
        if mix > 0.001 && !focused_media {
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
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
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
            // Playing video: a full-bleed, fading screen-space quad over the dimmed wall, exactly
            // like the photo lightbox. Audio has no video frame (it shows its cover in the lightbox
            // and just plays), so only draw the quad for actual videos.
            let is_video = matches!(self.focus.and_then(|i| self.sources.get(i)), Some(Source::Video(_)));
            if is_video {
                if let Some(v) = &self.video {
                    // Only draw the video once it has a real frame; before that the poster thumb
                    // (drawn below as the lightbox) stands in, so there's no black flash on open.
                    if v.has_frame() {
                        v.draw(&mut rp, self.video_rect_ndc(), smoothstep(self.focus_t), &self.queue);
                    }
                }
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
            // SVG icons (video controls, arrows, info) on top of the overlay + text.
            self.icons.draw(
                &mut rp,
                &self.queue,
                self.config.width as f32,
                self.config.height as f32,
                &icons,
            );
        }
        self.queue.submit(std::iter::once(enc.finish()));
        frame.present();
        Ok(())
    }
}

/// Caps total in-flight image-decode memory. A worker waits until its estimated decode fits the
/// budget (a single oversized image may always proceed alone), and releases on completion — so
/// several huge photos don't all decode at full resolution simultaneously and spike RSS.
struct DecodeBudget {
    used: std::sync::Mutex<usize>,
    cv: std::sync::Condvar,
    cap: usize,
}
impl DecodeBudget {
    fn new(cap: usize) -> Self {
        Self { used: std::sync::Mutex::new(0), cv: std::sync::Condvar::new(), cap }
    }
    fn acquire(&self, bytes: usize) {
        let mut used = self.used.lock().unwrap();
        while *used != 0 && *used + bytes > self.cap {
            used = self.cv.wait(used).unwrap();
        }
        *used += bytes;
    }
    fn release(&self, bytes: usize) {
        let mut used = self.used.lock().unwrap();
        *used = used.saturating_sub(bytes);
        self.cv.notify_all();
    }
}

/// Rough peak decode size (w·h·4) via a cheap header read; small/zero for non-image sources
/// (video/audio thumbnails go through a separate ffmpeg process; placeholders are tiny).
fn estimate_decode_bytes(source: &Source) -> usize {
    match source {
        Source::File(p) => image::image_dimensions(p)
            .map(|(w, h)| w as usize * h as usize * 4)
            .unwrap_or(8 * 1024 * 1024),
        _ => 0,
    }
}

/// The reference website's sample tiles (assets/samples/01.svg…18.svg), shown when no folder is
/// open — rasterised with resvg in place of the procedural placeholders.
const SAMPLES: [&str; 18] = [
    include_str!("../assets/samples/01.svg"),
    include_str!("../assets/samples/02.svg"),
    include_str!("../assets/samples/03.svg"),
    include_str!("../assets/samples/04.svg"),
    include_str!("../assets/samples/05.svg"),
    include_str!("../assets/samples/06.svg"),
    include_str!("../assets/samples/07.svg"),
    include_str!("../assets/samples/08.svg"),
    include_str!("../assets/samples/09.svg"),
    include_str!("../assets/samples/10.svg"),
    include_str!("../assets/samples/11.svg"),
    include_str!("../assets/samples/12.svg"),
    include_str!("../assets/samples/13.svg"),
    include_str!("../assets/samples/14.svg"),
    include_str!("../assets/samples/15.svg"),
    include_str!("../assets/samples/16.svg"),
    include_str!("../assets/samples/17.svg"),
    include_str!("../assets/samples/18.svg"),
];

/// usvg options with the system fonts loaded once (so SVG `<text>`, e.g. the sample numbers, renders).
/// The generic "sans-serif" family is mapped to a real installed font, else usvg can't resolve it.
fn svg_options() -> resvg::usvg::Options<'static> {
    use resvg::usvg::fontdb;
    static DB: std::sync::OnceLock<std::sync::Arc<fontdb::Database>> = std::sync::OnceLock::new();
    let db = DB
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let names: Vec<String> = db
                .faces()
                .flat_map(|f| f.families.iter().map(|(n, _)| n.clone()))
                .collect();
            // A Latin text font — skip CJK/symbol/mono faces (e.g. "Droid Sans Japanese" has no digits).
            let ok = |n: &str| {
                let l = n.to_lowercase();
                !["cjk", "japanese", "korean", "chinese", "thai", "arabic", "hebrew", "devanagari", "emoji", "symbol", "math", "mono"]
                    .iter()
                    .any(|k| l.contains(k))
            };
            let prefer = ["dejavu sans", "liberation sans", "noto sans", "carlito", "arimo", "arial", "roboto", "cantarell", "ubuntu", "open sans", "helvetica"];
            let fam = prefer
                .iter()
                .find_map(|p| names.iter().find(|n| { let l = n.to_lowercase(); l == *p || (l.starts_with(p) && ok(n)) }).cloned())
                .or_else(|| names.iter().find(|n| n.to_lowercase().contains("sans") && ok(n)).cloned())
                .or_else(|| names.into_iter().find(|n| ok(n)));
            if let Some(fam) = fam {
                db.set_sans_serif_family(fam);
            }
            std::sync::Arc::new(db)
        })
        .clone();
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb = db;
    opt
}

/// Rasterise an SVG to fit `target` px (preserving aspect). Returns (rgba, w, h); empty on failure.
fn rasterize_svg(svg: &str, target: u32) -> (Vec<u8>, u32, u32) {
    let opt = svg_options();
    let Ok(tree) = resvg::usvg::Tree::from_str(svg, &opt) else {
        return (Vec::new(), 0, 0);
    };
    let size = tree.size();
    let scale = (target as f32 / size.width()).min(target as f32 / size.height());
    let w = (size.width() * scale).round().max(1.0) as u32;
    let h = (size.height() * scale).round().max(1.0) as u32;
    let Some(mut pm) = resvg::tiny_skia::Pixmap::new(w, h) else {
        return (Vec::new(), 0, 0);
    };
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    (pm.data().to_vec(), w, h)
}

/// Decode + downscale one tile (runs on a worker thread). Resizes to fit TILE_PX preserving
/// aspect; returns (rgba, w, h). Empty/zero == failure.
fn decode(source: &Source, full: bool) -> (Vec<u8>, u32, u32) {
    let target = if full { FULL_PX } else { TILE_PX };
    match source {
        Source::File(p) => {
            // SVG isn't a raster format — rasterise it with resvg (the image crate can't decode it).
            if is_svg(p) {
                return std::fs::read_to_string(p)
                    .ok()
                    .map(|s| rasterize_svg(&s, target))
                    .unwrap_or((Vec::new(), 0, 0));
            }
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
        Source::Video(p) => {
            // Badge only on the wall thumbnail; the full-res lightbox poster is clean (no badge).
            video_thumb(p, target, !full).unwrap_or_else(|| (video_placeholder(), TILE_PX, TILE_PX))
        }
        Source::Audio(p) => {
            // Embedded cover art (ffmpeg reads the attached picture); else a music-note tile.
            cover_thumb(p, target).unwrap_or_else(|| (music_placeholder(), TILE_PX, TILE_PX))
        }
        Source::Placeholder(i) => {
            // The website's sample tiles (rasterised SVG); fall back to a procedural tile on failure.
            let (rgba, w, h) = rasterize_svg(SAMPLES[*i % SAMPLES.len()], target);
            if rgba.is_empty() {
                (placeholder(*i), TILE_PX, TILE_PX)
            } else {
                (rgba, w, h)
            }
        }
    }
}

/// Extract a poster frame from a video via ffmpeg, scaled to fit `target`, with a play badge drawn
/// on it. Returns None (→ fall back to the placeholder) if ffmpeg is missing or fails.
///
/// Tries a few seek points so more formats/clips yield a frame: ~1s in (skips black intros), then
/// the very start (short clips), then the `thumbnail` filter (scans for a representative frame).
fn video_thumb(p: &std::path::Path, target: u32, badge: bool) -> Option<(Vec<u8>, u32, u32)> {
    for ss in ["1", "0"] {
        if let Some(t) = ffmpeg_frame(p, target, Some(ss), false, badge) {
            return Some(t);
        }
    }
    ffmpeg_frame(p, target, None, true, badge)
}

/// `ffmpeg` command that never flashes a console window on Windows (CREATE_NO_WINDOW). We spawn it
/// many times for thumbnails/cover art, so without this the screen blinks with consoles on launch.
fn ffmpeg_cmd() -> std::process::Command {
    let cmd = std::process::Command::new("ffmpeg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        return cmd;
    }
    #[cfg(not(windows))]
    cmd
}

/// One ffmpeg poster-frame attempt. `ss` = input seek seconds (None = no seek); `pick` = use the
/// `thumbnail` filter to choose a representative (non-black) frame.
fn ffmpeg_frame(
    p: &std::path::Path,
    target: u32,
    ss: Option<&str>,
    pick: bool,
    badge: bool,
) -> Option<(Vec<u8>, u32, u32)> {
    let mut cmd = ffmpeg_cmd();
    cmd.args(["-nostdin", "-loglevel", "error"]);
    if let Some(s) = ss {
        cmd.args(["-ss", s]);
    }
    cmd.arg("-i").arg(p);
    let scale = format!("scale={target}:{target}:force_original_aspect_ratio=decrease");
    let vf = if pick { format!("thumbnail,{scale}") } else { scale };
    let out = cmd
        .args(["-frames:v", "1", "-an", "-sn", "-vf", &vf])
        .args(["-f", "image2pipe", "-vcodec", "png", "pipe:1"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let mut rgba = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    if badge {
        draw_play_badge(&mut rgba, w, h);
    }
    Some((rgba.into_raw(), w, h))
}

/// Extract embedded cover art from an audio file via ffmpeg (the attached picture is a video
/// stream). None → no art (fall back to the music placeholder).
fn cover_thumb(p: &std::path::Path, target: u32) -> Option<(Vec<u8>, u32, u32)> {
    let out = ffmpeg_cmd()
        .args(["-nostdin", "-loglevel", "error", "-i"])
        .arg(p)
        .args(["-frames:v", "1", "-vf"])
        .arg(format!(
            "scale={target}:{target}:force_original_aspect_ratio=decrease"
        ))
        .args(["-f", "image2pipe", "-vcodec", "png", "pipe:1"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let rgba = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((rgba.into_raw(), w, h))
}

/// A dark tile with a music note — the grid thumbnail for an audio file with no embedded cover.
fn music_placeholder() -> Vec<u8> {
    let mut buf = vec![0u8; (TILE_PX * TILE_PX * 4) as usize];
    let (cx, cy) = (TILE_PX as f32 * 0.42, TILE_PX as f32 * 0.62);
    let head_r = TILE_PX as f32 * 0.10;
    let stem_x = cx + head_r * 0.9;
    for y in 0..TILE_PX {
        for x in 0..TILE_PX {
            let i = ((y * TILE_PX + x) * 4) as usize;
            buf[i] = 26;
            buf[i + 1] = 24;
            buf[i + 2] = 34;
            buf[i + 3] = 255;
            let (fx, fy) = (x as f32, y as f32);
            let note = (fx - cx).hypot(fy - cy) < head_r // note head
                || (fx >= stem_x && fx < stem_x + head_r * 0.32 && fy > cy - head_r * 4.0 && fy < cy) // stem
                || (fx >= stem_x && fx < stem_x + head_r * 1.6 && fy > cy - head_r * 4.0 && fy < cy - head_r * 3.0); // flag
            if note {
                buf[i] = 225;
                buf[i + 1] = 225;
                buf[i + 2] = 235;
            }
        }
    }
    buf
}

/// Draw a play badge (a dim circle + white triangle) over the center of a thumbnail.
fn draw_play_badge(buf: &mut image::RgbaImage, w: u32, h: u32) {
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let r = w.min(h) as f32 * 0.15;
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            if dx * dx + dy * dy >= r * r {
                continue;
            }
            let px = buf.get_pixel_mut(x, y);
            // Play triangle (pointing right), else darken the circle behind it.
            if dx > -r * 0.4 && dx < r * 0.5 && dy.abs() < (r * 0.5 - dx) * 0.72 {
                *px = image::Rgba([245, 245, 250, 255]);
            } else {
                let c = px.0;
                *px = image::Rgba([c[0] / 2, c[1] / 2, c[2] / 2, 255]);
            }
        }
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
pub fn gather_sources(folder: Option<PathBuf>, progress: impl Fn(usize) + Sync) -> Vec<Source> {
    let Some(dir) = folder else {
        log::info!("no folder chosen — showing placeholders");
        return (0..18).map(Source::Placeholder).collect();
    };
    // Recurse into subfolders to any depth — media is usually nested (a folder per product/album).
    // ignore's parallel walker reads directories AND classifies across threads, so a big tree scans
    // fast. `progress` reports the running media count for the "Scanning… N found" readout.
    use ignore::{WalkBuilder, WalkState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let found = AtomicUsize::new(0);
    let items: std::sync::Mutex<Vec<(PathBuf, Kind)>> = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(4, 16);
    WalkBuilder::new(&dir)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .threads(threads)
        .build_parallel()
        .run(|| {
            Box::new(|res| {
                let Ok(e) = res else { return WalkState::Continue };
                if e.file_type().map_or(false, |t| t.is_file()) {
                    let p = e.into_path();
                    if let Some(kind) = classify(&p) {
                        let n = found.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % 500 == 0 {
                            progress(n);
                        }
                        items.lock().unwrap().push((p, kind));
                        if n >= 200_000 {
                            return WalkState::Quit;
                        }
                    }
                }
                WalkState::Continue
            })
        });
    let mut items = items.into_inner().unwrap();
    progress(items.len());
    items.sort_by(|a, b| a.0.cmp(&b.0)); // parallel order isn't stable — sort by path
    log::info!("folder {dir:?}: {} media files (incl. subfolders)", items.len());
    if items.is_empty() {
        log::info!("no images/videos under {dir:?} — showing placeholders");
        return (0..18).map(Source::Placeholder).collect();
    }
    items
        .into_iter()
        .map(|(p, kind)| match kind {
            Kind::Video => Source::Video(p),
            Kind::Audio => Source::Audio(p),
            Kind::Image => Source::File(p),
        })
        .collect()
}

/// Build a library from an explicit set of picked files (the Open button's file picker). Non-media
/// selections are dropped; the order picked is preserved (then the active sort applies on load).
pub fn gather_from_files(files: Vec<PathBuf>) -> Vec<Source> {
    files
        .into_iter()
        .filter_map(|p| match classify(&p) {
            Some(Kind::Video) => Some(Source::Video(p)),
            Some(Kind::Audio) => Some(Source::Audio(p)),
            Some(Kind::Image) => Some(Source::File(p)),
            None => None,
        })
        .collect()
}

/// Build a library from a JSON manifest (the Open dialog's "From JSON…"). The format is lenient:
/// any string anywhere in the JSON that resolves to an existing media file is loaded — so a bare
/// array of paths, an array of `{ "path"/"src"/"file": … }` objects, or `{ "items": [ … ] }` all
/// work. Relative paths resolve against the JSON file's own folder; http(s) URLs are skipped (this
/// is a local-file wall). Duplicates are removed, original order preserved.
pub fn gather_from_json(json_path: PathBuf) -> Vec<Source> {
    let Ok(text) = std::fs::read_to_string(&json_path) else {
        log::warn!("could not read JSON {json_path:?}");
        return Vec::new();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
        log::warn!("invalid JSON {json_path:?}");
        return Vec::new();
    };
    let base = json_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut strings = Vec::new();
    collect_json_strings(&val, &mut strings);
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in strings {
        if s.starts_with("http://") || s.starts_with("https://") {
            continue; // remote URLs aren't local files
        }
        let raw = PathBuf::from(&s);
        let p = if raw.is_absolute() { raw } else { base.join(raw) };
        if classify(&p).is_some() && p.is_file() && seen.insert(p.clone()) {
            files.push(p);
        }
    }
    log::info!("JSON {json_path:?}: {} media file(s)", files.len());
    gather_from_files(files)
}

/// Recursively collect every string value in a JSON tree (used by `gather_from_json`).
fn collect_json_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_json_strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_json_strings(x, out)),
        _ => {}
    }
}

// Extension groups for the Open file picker's type filters (kept in sync with `classify`).
pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "svg"];
pub const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];
pub const AUDIO_EXTS: &[&str] = &["mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma"];
pub const MEDIA_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "svg", "mp4", "mkv", "webm", "mov", "avi", "m4v",
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma",
];

enum Kind {
    Image,
    Video,
    Audio,
}

/// Classify a path by extension (None = ignore / not media).
fn is_gif(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("gif"))
        .unwrap_or(false)
}
fn is_svg(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("svg"))
        .unwrap_or(false)
}

/// Decode a GIF's frames (downscaled to fit `target`) with their delays, for in-place animation.
/// Streams one frame at a time — resize then drop the source frame — so peak memory is a single
/// source frame plus the (small) resized frames, not the whole GIF decoded at full size at once.
/// Runs on a worker thread; empty on failure.
fn decode_gif(path: &std::path::Path, target: u32, max_frames: usize) -> Vec<GifFrame> {
    use image::AnimationDecoder;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(dec) = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for frame in dec.into_frames() {
        let Ok(f) = frame else { break };
        let (n, d) = f.delay().numer_denom_ms();
        // A 0 / very-small frame delay means "use the default" — browsers render those at ~10fps,
        // not max speed. Clamping to 0.02 made such GIFs play ~5× too fast and skip frames on a
        // 60Hz display; treat anything under 20ms as 100ms.
        let raw = n as f32 / d.max(1) as f32 / 1000.0;
        let delay = if raw < 0.02 { 0.1 } else { raw };
        let buf = f.into_buffer();
        let (w, h) = (buf.width(), buf.height());
        let gf = if w > target || h > target {
            let img = image::DynamicImage::ImageRgba8(buf)
                .resize(target, target, image::imageops::FilterType::Triangle)
                .to_rgba8();
            let (w, h) = (img.width(), img.height());
            GifFrame { rgba: img.into_raw(), w, h, delay }
        } else {
            GifFrame { rgba: buf.into_raw(), w, h, delay }
        };
        out.push(gf);
        if out.len() >= max_frames {
            break;
        }
    }
    out
}

/// Decode a GIF and pack its frames into one 512×512 atlas (a grid of cells), for the wall. Streams
/// frames at a provisional size, picks the grid from the count, blits each into its cell. The atlas
/// is uploaded to the tile's layer once; animation is then just a UV shift — no per-frame uploads,
/// no stored frames. None on failure.
fn decode_gif_atlas(path: &std::path::Path) -> Option<GifAtlas> {
    use image::AnimationDecoder;
    let file = std::fs::File::open(path).ok()?;
    let dec = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)).ok()?;
    let mut frames: Vec<image::RgbaImage> = Vec::new();
    let mut delays: Vec<f32> = Vec::new();
    for frame in dec.into_frames() {
        let Ok(f) = frame else { break };
        let (n, d) = f.delay().numer_denom_ms();
        let raw = n as f32 / d.max(1) as f32 / 1000.0;
        delays.push(if raw < 0.02 { 0.1 } else { raw });
        let buf = f.into_buffer();
        let img = if buf.width() > WALL_GIF_PROV || buf.height() > WALL_GIF_PROV {
            image::DynamicImage::ImageRgba8(buf)
                .resize(WALL_GIF_PROV, WALL_GIF_PROV, image::imageops::FilterType::Triangle)
                .to_rgba8()
        } else {
            buf
        };
        frames.push(img);
        if frames.len() >= WALL_GIF_FRAMES {
            break;
        }
    }
    let count = frames.len();
    if count == 0 {
        return None;
    }
    let grid = (count as f64).sqrt().ceil() as u32; // ≤16 (count ≤ 256)
    let cell = (TILE_PX / grid).max(1);
    let total: f32 = delays.iter().sum::<f32>().max(0.01);
    let mut rgba = vec![0u8; (TILE_PX * TILE_PX * 4) as usize];
    let (mut fw, mut fh) = (0u32, 0u32);
    for (i, img) in frames.into_iter().enumerate() {
        let img = if img.width() > cell || img.height() > cell {
            image::DynamicImage::ImageRgba8(img)
                .resize(cell, cell, image::imageops::FilterType::Triangle)
                .to_rgba8()
        } else {
            img
        };
        if i == 0 {
            (fw, fh) = (img.width(), img.height());
        }
        let ox = (i as u32 % grid) * cell;
        let oy = (i as u32 / grid) * cell;
        blit_into(&mut rgba, TILE_PX, &img, ox, oy);
    }
    Some(GifAtlas { rgba, grid, cell, fw, fh, delays, total })
}

/// Copy `img` into the `aw`-pixel-wide RGBA buffer at pixel (ox, oy).
fn blit_into(dst: &mut [u8], aw: u32, img: &image::RgbaImage, ox: u32, oy: u32) {
    let (w, h) = (img.width(), img.height());
    let src = img.as_raw();
    let row = (w * 4) as usize;
    for y in 0..h {
        let d0 = (((oy + y) * aw + ox) * 4) as usize;
        let s0 = (y * w * 4) as usize;
        dst[d0..d0 + row].copy_from_slice(&src[s0..s0 + row]);
    }
}

/// Classify a path by extension (None = ignore / not media).
fn classify(p: &std::path::Path) -> Option<Kind> {
    match p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v") => Some(Kind::Video),
        Some("mp3" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wav" | "wma") => Some(Kind::Audio),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "svg") => Some(Kind::Image),
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

/// Map a click x (pixels) to a caret char-index, given the field's text origin and ~char width.
fn caret_from_x(x: f32, x0: f32, char_w: f32, len: usize) -> usize {
    if x <= x0 {
        return 0;
    }
    (((x - x0) / char_w).round() as usize).min(len)
}

/// Insert the (filtered) chars of `ch` into `s` at char-index `caret`; returns the new caret.
/// Date fields accept only digits and '-' and are capped at 10 chars (YYYY-MM-DD).
fn insert_into(s: &mut String, caret: usize, ch: &str, date: bool) -> usize {
    let mut caret = caret.min(s.chars().count());
    for c in ch.chars() {
        if c.is_control() {
            continue;
        }
        if date && !(c.is_ascii_digit() || c == '-') {
            continue;
        }
        if date && s.chars().count() >= 10 {
            break;
        }
        let byte = s.char_indices().nth(caret).map(|(b, _)| b).unwrap_or(s.len());
        s.insert(byte, c);
        caret += 1;
    }
    caret
}

/// Remove the char at char-index `idx` from `s`; returns whether anything was removed.
fn remove_at(s: &mut String, idx: usize) -> bool {
    if let Some((byte, _)) = s.char_indices().nth(idx) {
        s.remove(byte);
        true
    } else {
        false
    }
}

/// Howard Hinnant's days_from_civil: a civil (y, m, d) date → days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Parse "YYYY-MM-DD" → days since the Unix epoch (None unless it's a complete, plausible date).
fn parse_ymd_days(s: &str) -> Option<i64> {
    let mut it = s.trim().split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// A file's chosen date (modified, or created with a modified fallback) as days since the epoch.
fn file_days(p: &std::path::Path, created: bool) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?;
    let t = if created {
        m.created().or_else(|_| m.modified()).ok()?
    } else {
        m.modified().ok()?
    };
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(secs.div_euclid(86_400))
}

// glibc keeps memory freed by large transient allocations (full-size image decodes) instead of
// returning it to the OS, so RSS sits at the decode high-water mark. Ask it to give the slack back
// once a decode burst settles. No-op / absent off glibc.
#[cfg(target_os = "linux")]
extern "C" {
    fn malloc_trim(pad: usize) -> std::os::raw::c_int;
}
fn trim_heap() {
    #[cfg(target_os = "linux")]
    unsafe {
        malloc_trim(0);
    }
}

/// Resident set size (MB). Linux reads /proc/self/statm; Windows queries the working-set size.
/// None on other platforms (the readout simply hides).
#[cfg(target_os = "linux")]
fn process_rss_mb() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096 / (1024 * 1024))
}
#[cfg(target_os = "windows")]
fn process_rss_mb() -> Option<u64> {
    #[repr(C)]
    struct Pmc {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool: usize,
        quota_paged_pool: usize,
        quota_peak_nonpaged_pool: usize,
        quota_nonpaged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Pmc, cb: u32) -> i32;
    }
    unsafe {
        let mut pmc: Pmc = std::mem::zeroed();
        pmc.cb = std::mem::size_of::<Pmc>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
            Some(pmc.working_set_size as u64 / (1024 * 1024))
        } else {
            None
        }
    }
}
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn process_rss_mb() -> Option<u64> {
    None
}

/// A file's modified time as "M/D/YYYY" (UTC). Uses Howard Hinnant's civil-from-days algorithm so
/// we don't pull in a date crate just for the info card.
fn fmt_date(t: std::time::SystemTime) -> String {
    let secs = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return "—".into(),
    };
    let z = secs.div_euclid(86_400) + 719_468; // days since 0000-03-01
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = y + if m <= 2 { 1 } else { 0 };
    format!("{m}/{d}/{y}")
}

/// Point-in-rect test for a pixel-space (x, y, w, h) rect.
fn hit(rect: [f32; 4], x: f32, y: f32) -> bool {
    x >= rect[0] && x <= rect[0] + rect[2] && y >= rect[1] && y <= rect[1] + rect[3]
}

/// Sort a library by name or file date (modified/created). Decorate–sort–undecorate so each file's
/// name/date is read once, not on every comparison. `Default` keeps the as-loaded (scan) order.
fn sort_sources(srcs: Vec<Source>, mode: SortMode) -> Vec<Source> {
    let path = |s: &Source| match s {
        Source::File(p) | Source::Video(p) | Source::Audio(p) => Some(p.clone()),
        Source::Placeholder(_) => None,
    };
    match mode {
        SortMode::Default => srcs,
        SortMode::NameAsc | SortMode::NameDesc => {
            let mut keyed: Vec<(String, Source)> = srcs
                .into_iter()
                .map(|s| {
                    let k = path(&s)
                        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
                        .unwrap_or_default();
                    (k, s)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            if mode == SortMode::NameDesc {
                keyed.reverse();
            }
            keyed.into_iter().map(|(_, s)| s).collect()
        }
        _ => {
            // Date modes. `created()` isn't supported on every filesystem — fall back to modified.
            let created = matches!(mode, SortMode::CreatedNew | SortMode::CreatedOld);
            let newest_first = matches!(mode, SortMode::ModifiedNew | SortMode::CreatedNew);
            let mut keyed: Vec<(Option<std::time::SystemTime>, Source)> = srcs
                .into_iter()
                .map(|s| {
                    let k = path(&s).and_then(|p| std::fs::metadata(p).ok()).and_then(|m| {
                        if created {
                            m.created().or_else(|_| m.modified()).ok()
                        } else {
                            m.modified().ok()
                        }
                    });
                    (k, s)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            if newest_first {
                keyed.reverse();
            }
            keyed.into_iter().map(|(_, s)| s).collect()
        }
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
