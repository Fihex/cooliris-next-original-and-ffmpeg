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
mod ui;
mod video;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
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

struct App {
    state: Option<State>,
    cursor: (f64, f64),
    folder: Option<PathBuf>,
    folder_tx: Sender<PathBuf>,   // picker threads send the chosen folder here
    folder_rx: Receiver<PathBuf>, // polled each frame → reload
}

/// Open the native folder picker on a worker thread (blocking it inside the winit loop hangs /
/// fails on some Linux portals); the chosen folder comes back via the channel.
fn spawn_picker(tx: &Sender<PathBuf>) {
    let tx = tx.clone();
    log::info!("opening folder picker…");
    std::thread::spawn(move || {
        match rfd::FileDialog::new()
            .set_title("Open a photo / video folder")
            .pick_folder()
        {
            Some(dir) => {
                log::info!("picked folder: {dir:?}");
                let _ = tx.send(dir);
            }
            None => log::info!("folder picker cancelled / unavailable"),
        }
    });
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
        let state = pollster::block_on(State::new(window, self.folder.clone()));
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
            // Drag a folder (or a file) onto the window to load it.
            WindowEvent::DroppedFile(path) => {
                let folder = if path.is_dir() {
                    Some(path)
                } else {
                    path.parent().map(|p| p.to_path_buf())
                };
                if let Some(f) = folder {
                    if state.current_folder() != Some(f.as_path()) {
                        state.reload(Some(f));
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // web: deltaY positive (scroll down) zooms out; winit y is positive up → negate.
                // Scale line deltas up to roughly match pixel deltas.
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 100.0, -y * 100.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, -p.y as f32),
                };
                state.wheel(dx, dy);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                state.pointer_move(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                let code = match button {
                    MouseButton::Left => 0u8,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                let (cx, cy) = (self.cursor.0 as f32, self.cursor.1 as f32);
                if btn_state == ElementState::Pressed {
                    state.pointer_down(code, cx, cy);
                    if state.take_open_request() {
                        spawn_picker(&self.folder_tx);
                    }
                } else {
                    state.pointer_up(code);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                match event.physical_key {
                    // Arrows: prev/next in the lightbox, otherwise scroll the wall.
                    PhysicalKey::Code(KeyCode::ArrowRight) => {
                        if state.is_focused() {
                            if pressed {
                                state.navigate(1);
                            }
                        } else {
                            state.set_dir(if pressed { 1.0 } else { 0.0 });
                        }
                    }
                    PhysicalKey::Code(KeyCode::ArrowLeft) => {
                        if state.is_focused() {
                            if pressed {
                                state.navigate(-1);
                            }
                        } else {
                            state.set_dir(if pressed { -1.0 } else { 0.0 });
                        }
                    }
                    // O opens a folder picker at runtime.
                    PhysicalKey::Code(KeyCode::KeyO) if pressed => spawn_picker(&self.folder_tx),
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
                // A picker thread may have delivered a folder.
                while let Ok(folder) = self.folder_rx.try_recv() {
                    state.reload(Some(folder));
                }
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
    // Our logs at info; wgpu/naga are extremely chatty (info spam + benign startup warnings), so
    // silence them. Override anytime with RUST_LOG=…
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,cooliris_rs=info,wgpu_hal=error"),
    )
    .init();

    // Start the wall immediately (no forced dialog). Pass a folder on the CLI, or open one at
    // runtime via the toolbar's Open button, drag-and-drop, or the O key.
    let folder = std::env::args().nth(1).map(PathBuf::from);

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let (folder_tx, folder_rx) = std::sync::mpsc::channel();
    let mut app = App {
        state: None,
        cursor: (0.0, 0.0),
        folder,
        folder_tx,
        folder_rx,
    };
    event_loop.run_app(&mut app).expect("event loop error");
}
