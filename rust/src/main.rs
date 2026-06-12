// Cooliris (Rust/wgpu) — native rebuild of the 3D media wall.
//
// Why this exists: the Electron build's memory pain came entirely from embedding a browser
// (the protocol layer retaining image bytes, opaque GC, ~150MB baseline). Here the wall is a
// plain real-time GPU app: we own every texture's lifetime, decode off-thread, and idle in the
// tens of MB. This file is the foundation — a window + a configured wgpu surface that clears to
// the wall's near-black. The render pipeline, tile instancing, camera, image streaming and mpv
// video layer build on top of `State`.

use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

/// All GPU + surface state for the wall. Grows into pipelines, tile instance buffers, the
/// camera, and texture streaming. Held in an `Option` on `App` because winit only hands us a
/// window once the event loop is `resumed`.
struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
}

impl State {
    async fn new(window: Arc<Window>) -> State {
        let size = window.inner_size();

        // PRIMARY = Vulkan/Metal/DX12 (skip the GL fallback unless those are unavailable).
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        // The surface borrows the window for as long as it lives; an Arc<Window> makes that
        // 'static so State can own both without lifetime gymnastics.
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

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

        // Prefer an sRGB swapchain format so colours match the source images.
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
            present_mode: wgpu::PresentMode::Fifo, // vsync — smooth scroll, no tearing
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        log::info!(
            "GPU ready: {} ({:?}), surface {}x{} {:?}",
            adapter.get_info().name,
            adapter.get_info().backend,
            config.width,
            config.height,
            format
        );

        State {
            window,
            surface,
            device,
            queue,
            config,
            size,
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.size = size;
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });
        {
            // For now just clear to the wall's near-black. Tile draws will go in this pass.
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}

/// winit 0.30 application: owns the window/GPU state and routes events into it.
#[derive(Default)]
struct App {
    state: Option<State>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return; // already initialised (e.g. resumed after suspend)
        }
        let attrs = Window::default_attributes()
            .with_title("Cooliris (rs)")
            .with_inner_size(PhysicalSize::new(1440, 900));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        let state = pollster::block_on(State::new(window));
        state.window.request_redraw();
        self.state = Some(state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::RedrawRequested => {
                match state.render() {
                    Ok(()) => {}
                    // Surface needs reconfiguring (resize/minimise race) — rebuild and retry next frame.
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        state.resize(state.size)
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                    Err(e) => log::warn!("surface error: {e:?}"),
                }
                state.window.request_redraw(); // keep the loop pumping (we'll gate this later)
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = EventLoop::new().expect("failed to create event loop");
    // Poll = run as fast as vsync allows (we redraw continuously for now).
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::default();
    event_loop.run_app(&mut app).expect("event loop error");
}
