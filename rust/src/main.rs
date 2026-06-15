// On Windows release builds, use the GUI subsystem so double-clicking the .exe doesn't pop a
// console window. Debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Cooliris (Rust/wgpu) — native rebuild of the 3D media wall.
//
// Why this exists: the Electron build's memory pain came entirely from embedding a browser.
// Here the wall is a plain real-time GPU app — we own every texture's lifetime, decode off the
// main thread, and idle in tens of MB. `main` owns the winit event loop and routes input into
// `State` (the wgpu device, pipeline, texture array, camera and wall layout).
//
// Run: `cargo run --release -- /path/to/photos`  (no path → placeholder tiles).
// Controls: mouse wheel or ←/→ to scroll the wall.

mod components;
mod icons;
mod post;
mod state;
mod ui;
mod video;

// Return freed memory (big image-decode buffers) to the OS instead of letting glibc retain the
// high-water mark — keeps RSS from sitting hundreds of MB above what's actually live.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Fullscreen, Window, WindowId},
};

use state::State;

/// Message from a picker/scan worker thread.
enum LoadMsg {
    Library(PathBuf, Vec<state::Source>), // a scanned folder
    ScanProgress(usize),                  // media files found so far (scan in progress)
    Cancelled,                            // the picker was dismissed
}

struct App {
    state: Option<State>,
    cursor: (f64, f64),
    folder: Option<PathBuf>,
    folder_tx: Sender<LoadMsg>,   // picker/scan threads send results here
    folder_rx: Receiver<LoadMsg>, // polled each frame
}

/// Open the native folder picker on a worker thread (blocking it inside the winit loop hangs /
/// fails on some Linux portals), then scan the folder there too (a big/slow tree shouldn't freeze
/// the window). The scanned library comes back via the channel.
fn spawn_picker(tx: &Sender<LoadMsg>) {
    let tx = tx.clone();
    log::info!("opening folder picker…");
    std::thread::spawn(move || {
        match rfd::FileDialog::new()
            .set_title("Open a photo / video folder")
            .pick_folder()
        {
            Some(dir) => {
                log::info!("picked folder: {dir:?}");
                let sources =
                    state::gather_sources(Some(dir.clone()), |n| {
                        let _ = tx.send(LoadMsg::ScanProgress(n));
                    });
                let _ = tx.send(LoadMsg::Library(dir, sources));
            }
            None => {
                log::info!("folder picker cancelled / unavailable");
                let _ = tx.send(LoadMsg::Cancelled);
            }
        }
    });
}

/// Open the native FILE picker on a worker thread (multi-select: images, video, music, GIFs).
/// The Open button uses this; the O key still opens a whole folder. Picked files become the
/// library directly — handy for opening a single clip or a hand-picked set.
fn spawn_file_picker(tx: &Sender<LoadMsg>) {
    let tx = tx.clone();
    log::info!("opening file picker…");
    std::thread::spawn(move || {
        match rfd::FileDialog::new()
            .add_filter("All media", state::MEDIA_EXTS)
            .add_filter("Images", state::IMAGE_EXTS)
            .add_filter("Video", state::VIDEO_EXTS)
            .add_filter("Music", state::AUDIO_EXTS)
            .add_filter("All files", &["*"])
            .set_title("Open image / video / music files")
            .pick_files()
        {
            Some(files) if !files.is_empty() => {
                let sources = state::gather_from_files(files.clone());
                if sources.is_empty() {
                    let _ = tx.send(LoadMsg::Cancelled);
                } else {
                    log::info!("picked {} media file(s)", sources.len());
                    let folder = files[0]
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| files[0].clone());
                    let _ = tx.send(LoadMsg::Library(folder, sources));
                }
            }
            _ => {
                log::info!("file picker cancelled / unavailable");
                let _ = tx.send(LoadMsg::Cancelled);
            }
        }
    });
}

/// Open a JSON-manifest file picker on a worker thread (the Open dialog's "From JSON…"), then load
/// the media paths it lists. The resulting set becomes the library.
fn spawn_json_picker(tx: &Sender<LoadMsg>) {
    let tx = tx.clone();
    log::info!("opening JSON picker…");
    std::thread::spawn(move || {
        match rfd::FileDialog::new()
            .add_filter("JSON manifest", &["json"])
            .add_filter("All files", &["*"])
            .set_title("Open a JSON media manifest")
            .pick_file()
        {
            Some(path) => {
                let sources = state::gather_from_json(path.clone());
                if sources.is_empty() {
                    log::info!("JSON had no loadable media");
                    let _ = tx.send(LoadMsg::Cancelled);
                } else {
                    let folder = path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| path.clone());
                    let _ = tx.send(LoadMsg::Library(folder, sources));
                }
            }
            None => {
                log::info!("JSON picker cancelled / unavailable");
                let _ = tx.send(LoadMsg::Cancelled);
            }
        }
    });
}

/// Scan a folder (e.g. a drag-and-dropped one) on a worker thread.
fn spawn_scan(tx: &Sender<LoadMsg>, dir: PathBuf) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let sources = state::gather_sources(Some(dir.clone()), |n| {
            let _ = tx.send(LoadMsg::ScanProgress(n));
        });
        let _ = tx.send(LoadMsg::Library(dir, sources));
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
                state.close_menu(); // dismiss the Open dialog if it's up
                let folder = if path.is_dir() {
                    Some(path)
                } else {
                    path.parent().map(|p| p.to_path_buf())
                };
                if let Some(f) = folder {
                    if state.current_folder() != Some(f.as_path()) {
                        state.set_scanning(true);
                        spawn_scan(&self.folder_tx, f);
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
                    match state.take_open_request() {
                        Some(state::OpenKind::Files) => {
                            state.set_scanning(true);
                            spawn_file_picker(&self.folder_tx);
                        }
                        Some(state::OpenKind::Folder) => {
                            state.set_scanning(true);
                            spawn_picker(&self.folder_tx);
                        }
                        Some(state::OpenKind::Json) => {
                            state.set_scanning(true);
                            spawn_json_picker(&self.folder_tx);
                        }
                        None => {}
                    }
                    if state.take_fullscreen_request() {
                        let fs = match state.window.fullscreen() {
                            Some(_) => None,
                            None => Some(Fullscreen::Borderless(None)),
                        };
                        state.window.set_fullscreen(fs);
                    }
                } else {
                    state.pointer_up(code);
                }
            }
            // While a text field (search box or a Dates field) is focused, the keyboard edits it
            // at the caret — arrows/Home/End move, Backspace/Delete edit, Enter/Esc leave.
            WindowEvent::KeyboardInput { event, .. }
                if state.input_active() && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                match &event.logical_key {
                    Key::Named(NamedKey::Backspace) => state.backspace(),
                    Key::Named(NamedKey::Delete) => state.delete_forward(),
                    Key::Named(NamedKey::ArrowLeft) => state.caret_left(),
                    Key::Named(NamedKey::ArrowRight) => state.caret_right(),
                    Key::Named(NamedKey::Home) => state.caret_home(),
                    Key::Named(NamedKey::End) => state.caret_end(),
                    Key::Named(NamedKey::Enter | NamedKey::Escape) => state.input_done(),
                    _ => {
                        if let Some(t) = &event.text {
                            state.input_char(t);
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                match event.physical_key {
                    // Arrows: on a focused video they seek ±10s; on a focused image they go
                    // prev/next; on the wall they scroll.
                    PhysicalKey::Code(KeyCode::ArrowRight) => {
                        if state.is_focused() {
                            if pressed {
                                if state.focused_is_video() {
                                    state.video_command(&["seek", "10"]);
                                } else {
                                    state.navigate(1);
                                }
                            }
                        } else {
                            state.set_dir(if pressed { 1.0 } else { 0.0 });
                        }
                    }
                    PhysicalKey::Code(KeyCode::ArrowLeft) => {
                        if state.is_focused() {
                            if pressed {
                                if state.focused_is_video() {
                                    state.video_command(&["seek", "-10"]);
                                } else {
                                    state.navigate(-1);
                                }
                            }
                        } else {
                            state.set_dir(if pressed { -1.0 } else { 0.0 });
                        }
                    }
                    // Up/Down adjust the playing video/audio volume.
                    PhysicalKey::Code(KeyCode::ArrowUp) if pressed => {
                        state.video_command(&["add", "volume", "5"]);
                    }
                    PhysicalKey::Code(KeyCode::ArrowDown) if pressed => {
                        state.video_command(&["add", "volume", "-5"]);
                    }
                    // O opens a folder picker at runtime.
                    PhysicalKey::Code(KeyCode::KeyO) if pressed => {
                        state.set_scanning(true);
                        spawn_picker(&self.folder_tx);
                    }
                    // I toggles the item info panel.
                    PhysicalKey::Code(KeyCode::KeyI) if pressed => state.toggle_info(),
                    // F toggles borderless fullscreen (wall, lightbox, or video).
                    PhysicalKey::Code(KeyCode::KeyF) if pressed => {
                        let fs = match state.window.fullscreen() {
                            Some(_) => None,
                            None => Some(Fullscreen::Borderless(None)),
                        };
                        state.window.set_fullscreen(fs);
                    }
                    // Video controls (only act on a focused video; need --features video to play).
                    PhysicalKey::Code(KeyCode::Space) if pressed => {
                        state.video_command(&["cycle", "pause"]);
                    }
                    PhysicalKey::Code(KeyCode::KeyA) if pressed => {
                        state.video_command(&["cycle", "aid"]); // next audio track
                    }
                    PhysicalKey::Code(KeyCode::KeyS) if pressed => {
                        state.video_command(&["cycle", "sid"]); // next subtitle track
                    }
                    // Esc: leave the lightbox, else exit fullscreen, else quit.
                    PhysicalKey::Code(KeyCode::Escape) if pressed => {
                        if state.is_focused() {
                            state.back();
                        } else if state.window.fullscreen().is_some() {
                            state.window.set_fullscreen(None);
                        } else {
                            event_loop.exit();
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                // A picker/scan thread may have delivered a result.
                while let Ok(msg) = self.folder_rx.try_recv() {
                    match msg {
                        LoadMsg::Library(folder, sources) => state.reload_with(Some(folder), sources),
                        LoadMsg::ScanProgress(n) => state.set_scan_count(n),
                        LoadMsg::Cancelled => state.set_scanning(false),
                    }
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
        env_logger::Env::default()
            .default_filter_or("warn,cooliris_rs=info,wgpu_hal=error,wgpu_core=error"),
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
