// Cooliris (Rust/wgpu) — native rebuild of the 3D media wall.
//
// Why this exists: the Electron build's memory pain came entirely from embedding a browser.
// Here the wall is a plain real-time GPU app — we own every texture's lifetime, decode off the
// main thread, and idle in tens of MB. `main` owns the winit event loop and routes input into
// `State` (the wgpu device, pipeline, texture array, camera and wall layout).
//
// Run: `cargo run --release -- /path/to/photos`  (no path → placeholder tiles).
// Controls: mouse wheel or ←/→ to scroll the wall.

mod state;
mod video;

use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

use state::State;

#[derive(Default)]
struct App {
    state: Option<State>,
    cursor: (f64, f64),
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
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
            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x + y) * 0.5,
                    MouseScrollDelta::PixelDelta(p) => (p.x + p.y) as f32 * 0.01,
                };
                state.scroll(d);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => state.click(self.cursor.0 as f32, self.cursor.1 as f32),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::ArrowRight) => {
                        state.set_dir(if pressed { 1.0 } else { 0.0 })
                    }
                    PhysicalKey::Code(KeyCode::ArrowLeft) => {
                        state.set_dir(if pressed { -1.0 } else { 0.0 })
                    }
                    // Esc returns from focus, then (if already on the wall) quits.
                    PhysicalKey::Code(KeyCode::Escape) if pressed => {
                        if state.is_focused() {
                            state.back();
                        } else {
                            event_loop.exit();
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                state.update();
                match state.render() {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        state.resize(state.window.inner_size())
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                    Err(e) => log::warn!("surface error: {e:?}"),
                }
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::default();
    event_loop.run_app(&mut app).expect("event loop error");
}
