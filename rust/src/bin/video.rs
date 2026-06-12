// libmpv → wgpu video proof.
//
// The Electron build needed a whole second transparent child window and `--wid` HWND embedding
// to get native video on screen. Natively we just ask mpv's *software* render API for the
// current frame as RGBA bytes, upload them to a wgpu texture, and draw a fullscreen quad — mpv
// decodes (hardware-accelerated internally) and we composite it ourselves. No window-in-window,
// no interop. This binary proves the pipeline end to end; the wall integrates it as video tiles.
//
// Run: `cargo run --features video --bin video -- /path/to/clip.mp4`

use std::ffi::{c_char, c_int, c_void, CString};
use std::ptr;
use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

// Fixed software-render target (mpv scales the video to fit, letterboxing). 1280*4 = 5120-byte
// stride (a multiple of 64, as mpv prefers).
const W: usize = 1280;
const H: usize = 720;

/* ----------------------------- minimal libmpv FFI ----------------------------- */

#[repr(C)]
struct MpvHandle {
    _p: [u8; 0],
}
#[repr(C)]
struct MpvRenderContext {
    _p: [u8; 0],
}
#[repr(C)]
struct MpvRenderParam {
    type_: c_int,
    data: *mut c_void,
}
#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

const MPV_RENDER_PARAM_INVALID: c_int = 0;
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

extern "C" {
    fn mpv_create() -> *mut MpvHandle;
    fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
    fn mpv_set_option_string(ctx: *mut MpvHandle, name: *const c_char, data: *const c_char)
        -> c_int;
    fn mpv_command(ctx: *mut MpvHandle, args: *const *const c_char) -> c_int;
    fn mpv_wait_event(ctx: *mut MpvHandle, timeout: f64) -> *mut MpvEvent;
    fn mpv_render_context_create(
        res: *mut *mut MpvRenderContext,
        mpv: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_render(ctx: *mut MpvRenderContext, params: *mut MpvRenderParam) -> c_int;
}

/// Owns the mpv core + render context and renders the current frame into a CPU buffer.
struct Mpv {
    handle: *mut MpvHandle,
    render: *mut MpvRenderContext,
    buf: Vec<u32>, // W*H, 4-byte aligned for the SW renderer
}

impl Mpv {
    fn new(path: &str) -> Mpv {
        unsafe {
            let handle = mpv_create();
            assert!(!handle.is_null(), "mpv_create failed");
            // Keep it simple + robust in a headless dev box.
            // Route video output through the render API (the render context becomes the VO).
            // Without this mpv opens its own gpu-next window and never feeds our SW buffer.
            mpv_set_option_string(handle, c"vo".as_ptr(), c"libmpv".as_ptr());
            mpv_set_option_string(handle, c"terminal".as_ptr(), c"no".as_ptr());
            mpv_set_option_string(handle, c"loop".as_ptr(), c"inf".as_ptr());
            assert!(mpv_initialize(handle) >= 0, "mpv_initialize failed");

            // Software render context.
            let mut params = [
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_API_TYPE,
                    data: c"sw".as_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            let mut render: *mut MpvRenderContext = ptr::null_mut();
            let rc = mpv_render_context_create(&mut render, handle, params.as_mut_ptr());
            assert!(rc >= 0 && !render.is_null(), "render_context_create failed");

            // Start playback.
            let cpath = CString::new(path).unwrap();
            let cmd: [*const c_char; 3] = [c"loadfile".as_ptr(), cpath.as_ptr(), ptr::null()];
            mpv_command(handle, cmd.as_ptr());

            Mpv {
                handle,
                render,
                buf: vec![0u32; W * H],
            }
        }
    }

    /// Drain mpv's event queue so the core keeps progressing (it can stall if events pile up).
    fn pump(&self) {
        unsafe {
            loop {
                let ev = mpv_wait_event(self.handle, 0.0);
                if ev.is_null() || (*ev).event_id == 0 {
                    break; // MPV_EVENT_NONE
                }
            }
        }
    }

    /// Render the current frame into `buf`; returns the bytes (RGBA, top-left origin).
    fn frame(&mut self) -> &[u8] {
        let mut size = [W as c_int, H as c_int];
        let mut stride: usize = W * 4;
        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_SW_FORMAT,
                data: c"rgb0".as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_SW_STRIDE,
                data: &mut stride as *mut usize as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_SW_POINTER,
                data: self.buf.as_mut_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        unsafe {
            mpv_render_context_render(self.render, params.as_mut_ptr());
        }
        bytemuck::cast_slice(&self.buf)
    }
}

// mpv lives on the main thread for the whole program; we never move it across threads.
unsafe impl Send for Mpv {}

/* --------------------------------- wgpu output -------------------------------- */

const SHADER: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    // one fullscreen triangle
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let pos = p[i];
    var out: V;
    out.clip = vec4(pos, 0.0, 1.0);
    out.uv = vec2((pos.x + 1.0) * 0.5, (1.0 - pos.y) * 0.5); // flip Y (image top-left origin)
    return out;
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    return vec4(textureSample(t, s, in.uv).rgb, 1.0); // ignore mpv's garbage 4th byte
}
"#;

struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    mpv: Mpv,
    frames: u64,
}

impl Gpu {
    async fn new(window: Arc<Window>, mpv: Mpv) -> Gpu {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .unwrap();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .unwrap();

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

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video"),
            size: wgpu::Extent3d {
                width: W as u32,
                height: H as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
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
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(config.format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Gpu {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            tex,
            bind,
            mpv,
            frames: 0,
        }
    }

    fn render(&mut self) {
        // Pull the current frame from mpv and upload it.
        self.mpv.pump();
        let pixels = self.mpv.frame();
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W as u32 * 4),
                rows_per_image: Some(H as u32),
            },
            wgpu::Extent3d {
                width: W as u32,
                height: H as u32,
                depth_or_array_layers: 1,
            },
        );

        if log::log_enabled!(log::Level::Debug) && self.frames % 120 == 0 {
            let nonzero = self.mpv.buf.iter().filter(|&&p| p != 0).count();
            log::debug!("frame {} — non-zero pixels: {}/{}", self.frames, nonzero, W * H);
        }
        self.frames += 1;

        let Ok(frame) = self.surface.get_current_texture() else {
            return;
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &self.bind, &[]);
            rp.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(enc.finish()));
        frame.present();
    }
}

#[derive(Default)]
struct App {
    gpu: Option<Gpu>,
    path: String,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Cooliris video (rs)")
            .with_inner_size(PhysicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        let mpv = Mpv::new(&self.path);
        let gpu = pollster::block_on(Gpu::new(window, mpv));
        gpu.window.request_redraw();
        self.gpu = Some(gpu);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gpu.config.width = size.width.max(1);
                gpu.config.height = size.height.max(1);
                gpu.surface.configure(&gpu.device, &gpu.config);
            }
            WindowEvent::RedrawRequested => {
                gpu.render();
                gpu.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: video <file>");
        std::process::exit(2);
    };
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        gpu: None,
        path,
    };
    event_loop.run_app(&mut app).unwrap();
}
