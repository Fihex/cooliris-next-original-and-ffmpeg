// Video playback for a focused wall tile. The hard part (libmpv's software render API → a wgpu
// texture, no second window / no interop) is proven in src/bin/video.rs; here it's packaged as a
// `Player` that draws onto a positioned quad using the wall camera.
//
// Feature-gated: with `--features video` this is the real libmpv player; without it, a no-op stub
// so the wall still builds with no libmpv dependency. `State` always holds an `Option<Player>`.

#[cfg(not(feature = "video"))]
pub use stub::Player;
#[cfg(feature = "video")]
pub use real::Player;

/// One audio or subtitle track, for the track-selection menus.
pub struct Track {
    pub id: i64,
    pub audio: bool, // true = audio track, false = subtitle track
    pub label: String,
    pub selected: bool,
}

/// No-op player for builds without the `video` feature — focusing a video tile just shows the
/// placeholder + camera zoom.
#[cfg(not(feature = "video"))]
mod stub {
    pub struct Player;
    impl Player {
        pub fn start(
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _format: wgpu::TextureFormat,
            _path: &std::path::Path,
        ) -> Player {
            Player
        }
        pub fn update(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue, _w: u32, _h: u32) {}
        pub fn has_frame(&self) -> bool {
            false
        }
        pub fn command(&self, _args: &[&str]) {}
        pub fn position(&self) -> f64 {
            0.0
        }
        pub fn duration(&self) -> f64 {
            0.0
        }
        pub fn volume(&self) -> f64 {
            100.0
        }
        pub fn paused(&self) -> bool {
            false
        }
        pub fn seek(&self, _secs: f64) {}
        pub fn aid(&self) -> i64 {
            0
        }
        pub fn sid(&self) -> i64 {
            0
        }
        pub fn tracks(&self) -> Vec<super::Track> {
            Vec::new()
        }
        pub fn draw<'a>(
            &'a self,
            _rp: &mut wgpu::RenderPass<'a>,
            _rect_ndc: [f32; 4],
            _alpha: f32,
            _queue: &wgpu::Queue,
        ) {
        }
    }
}

#[cfg(feature = "video")]
mod real {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::path::Path;
    use std::ptr;

    // Initial SW-render surface; resized to the display aspect on the first `update` so mpv fits
    // the clip (letterboxing) and places subtitles for the actual screen, not a fixed 16:9.
    const INIT_W: u32 = 1280;
    const INIT_H: u32 = 720;
    const MAX_DIM: u32 = 1600; // cap the longer side (software render cost ∝ pixels)

    /// Map an ISO-639 language code (e.g. "eng", "ja") to a full name. Unknown codes pass through,
    /// so every track shows *something* real rather than a bare number.
    fn lang_full_name(code: &str) -> String {
        if code.is_empty() {
            return String::new();
        }
        let name = match code.to_ascii_lowercase().as_str() {
            "en" | "eng" => "English",
            "ja" | "jpn" => "Japanese",
            "es" | "spa" => "Spanish",
            "fr" | "fre" | "fra" => "French",
            "de" | "ger" | "deu" => "German",
            "it" | "ita" => "Italian",
            "ru" | "rus" => "Russian",
            "zh" | "chi" | "zho" => "Chinese",
            "ko" | "kor" => "Korean",
            "pt" | "por" => "Portuguese",
            "ar" | "ara" => "Arabic",
            "hi" | "hin" => "Hindi",
            "nl" | "dut" | "nld" => "Dutch",
            "pl" | "pol" => "Polish",
            "tr" | "tur" => "Turkish",
            "sv" | "swe" => "Swedish",
            "no" | "nor" => "Norwegian",
            "da" | "dan" => "Danish",
            "fi" | "fin" => "Finnish",
            "cs" | "cze" | "ces" => "Czech",
            "el" | "gre" | "ell" => "Greek",
            "he" | "heb" => "Hebrew",
            "th" | "tha" => "Thai",
            "vi" | "vie" => "Vietnamese",
            "uk" | "ukr" => "Ukrainian",
            "hu" | "hun" => "Hungarian",
            "ro" | "rum" | "ron" => "Romanian",
            "id" | "ind" => "Indonesian",
            _ => return code.to_string(),
        };
        name.to_string()
    }

    /// Clamp a display size to the render budget, preserving aspect (keeps subtitle placement and
    /// letterboxing correct). Even dimensions keep the row stride tidy.
    fn cap_dims(w: u32, h: u32) -> (u32, u32) {
        let (w, h) = (w.max(16), h.max(16));
        let scale = (MAX_DIM as f32 / w.max(h) as f32).min(1.0);
        let cw = (((w as f32 * scale) as u32).max(16)) & !1;
        let ch = (((h as f32 * scale) as u32).max(16)) & !1;
        (cw, ch)
    }

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
    const SW_SIZE: c_int = 17;
    const SW_FORMAT: c_int = 18;
    const SW_STRIDE: c_int = 19;
    const SW_POINTER: c_int = 20;
    const API_TYPE: c_int = 1;
    const INVALID: c_int = 0;
    const FORMAT_FLAG: c_int = 3; // MPV_FORMAT_FLAG  (int*)
    const FORMAT_INT64: c_int = 4; // MPV_FORMAT_INT64 (int64*)
    const FORMAT_DOUBLE: c_int = 5; // MPV_FORMAT_DOUBLE (double*)

    extern "C" {
        fn mpv_create() -> *mut MpvHandle;
        fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
        // Fully shuts the player down (stops playback/audio) and waits — unlike mpv_destroy, which
        // only detaches the handle and can leave the core (and its audio) running.
        fn mpv_terminate_destroy(ctx: *mut MpvHandle);
        fn mpv_set_option_string(
            ctx: *mut MpvHandle,
            name: *const c_char,
            data: *const c_char,
        ) -> c_int;
        fn mpv_command(ctx: *mut MpvHandle, args: *const *const c_char) -> c_int;
        fn mpv_get_property(
            ctx: *mut MpvHandle,
            name: *const c_char,
            format: c_int,
            data: *mut c_void,
        ) -> c_int;
        fn mpv_get_property_string(ctx: *mut MpvHandle, name: *const c_char) -> *mut c_char;
        fn mpv_free(data: *mut c_void);
        fn mpv_wait_event(ctx: *mut MpvHandle, timeout: f64) -> *mut MpvEvent;
        fn mpv_render_context_create(
            res: *mut *mut MpvRenderContext,
            mpv: *mut MpvHandle,
            params: *mut MpvRenderParam,
        ) -> c_int;
        fn mpv_render_context_render(
            ctx: *mut MpvRenderContext,
            params: *mut MpvRenderParam,
        ) -> c_int;
        fn mpv_render_context_free(ctx: *mut MpvRenderContext);
    }

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Rect {
        rect: [f32; 4],  // NDC: bottom-left x, y + width, height
        alpha: [f32; 4], // .x = fade-in alpha (vec4 for 16-byte alignment)
    }

    // Screen-space quad (no camera): the video is drawn like the photo lightbox — a fitted, fading
    // NDC rect over the dimmed wall. mpv has already laid the frame (and subtitles) into the surface.
    const SHADER: &str = r#"
@group(0) @binding(0) var vid: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct U { rect: vec4<f32>, alpha: vec4<f32> };
@group(1) @binding(0) var<uniform> u: U;

struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    var corners = array<vec2<f32>, 6>(
        vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let c = corners[i];
    let p = u.rect.xy + c * u.rect.zw; // bottom-left + size, in NDC
    var out: V;
    out.clip = vec4(p, 0.0, 1.0);
    out.uv = vec2(c.x, 1.0 - c.y);
    return out;
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    return vec4(textureSample(vid, samp, in.uv).rgb, u.alpha.x);
}
"#;

    pub struct Player {
        mpv: *mut MpvHandle,
        render: *mut MpvRenderContext,
        buf: Vec<u32>,
        tw: u32, // current render-surface size (resized to the display aspect)
        th: u32,
        tex: wgpu::Texture,
        tex_bgl: wgpu::BindGroupLayout, // kept to rebuild tex_bg when the surface resizes
        sampler: wgpu::Sampler,
        pipeline: wgpu::RenderPipeline,
        tex_bg: wgpu::BindGroup,
        rect_buf: wgpu::Buffer,
        rect_bg: wgpu::BindGroup,
        has_frame: bool, // true once mpv has produced a real (non-black) frame
    }

    impl Player {
        pub fn start(
            device: &wgpu::Device,
            _queue: &wgpu::Queue,
            format: wgpu::TextureFormat,
            path: &Path,
        ) -> Player {
            // mpv core + SW render context.
            let mpv = unsafe {
                let h = mpv_create();
                mpv_set_option_string(h, c"vo".as_ptr(), c"libmpv".as_ptr());
                mpv_set_option_string(h, c"terminal".as_ptr(), c"no".as_ptr());
                // Play once and hold the final frame (paused) at the end — no looping.
                mpv_set_option_string(h, c"keep-open".as_ptr(), c"yes".as_ptr());
                // Auto-load every external subtitle in the video's folder (not just exact/likely
                // name matches), so a same-folder .srt/.ass always shows up and is selectable.
                mpv_set_option_string(h, c"sub-auto".as_ptr(), c"all".as_ptr());
                mpv_initialize(h);
                h
            };
            let mut params = [
                MpvRenderParam {
                    type_: API_TYPE,
                    data: c"sw".as_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: INVALID,
                    data: ptr::null_mut(),
                },
            ];
            let mut render: *mut MpvRenderContext = ptr::null_mut();
            unsafe {
                mpv_render_context_create(&mut render, mpv, params.as_mut_ptr());
                let cpath = CString::new(path.to_string_lossy().as_ref()).unwrap();
                let cmd: [*const c_char; 3] = [c"loadfile".as_ptr(), cpath.as_ptr(), ptr::null()];
                mpv_command(mpv, cmd.as_ptr());
            }

            // wgpu side: video texture + sampler + screen-space pipeline.
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video"),
                size: wgpu::Extent3d {
                    width: INIT_W,
                    height: INIT_H,
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
            let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &tex_bgl,
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

            let rect_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("video-rect"),
                size: std::mem::size_of::<Rect>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let rect_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Read in both stages: vs uses u.rect, fs uses u.alpha.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
            let rect_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &rect_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rect_buf.as_entire_binding(),
                }],
            });

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&tex_bgl, &rect_bgl],
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
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

            Player {
                mpv,
                render,
                buf: vec![0u32; (INIT_W * INIT_H) as usize],
                tw: INIT_W,
                th: INIT_H,
                tex,
                tex_bgl,
                sampler,
                pipeline,
                tex_bg,
                rect_buf,
                rect_bg,
                has_frame: false,
            }
        }

        /// True once mpv has rendered a real (non-black) frame — until then the lightbox shows the
        /// poster thumbnail, so opening a video feels as instant as opening an image.
        pub fn has_frame(&self) -> bool {
            self.has_frame
        }

        /// Send an mpv command (NULL-terminated argv), e.g. ["cycle","pause"] / ["cycle","aid"].
        pub fn command(&self, args: &[&str]) {
            unsafe {
                let cstrs: Vec<CString> = args
                    .iter()
                    .filter_map(|a| CString::new(*a).ok())
                    .collect();
                let mut ptrs: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
                ptrs.push(ptr::null());
                mpv_command(self.mpv, ptrs.as_ptr());
            }
        }

        fn get_double(&self, name: &[u8]) -> f64 {
            let mut out: f64 = 0.0;
            unsafe {
                mpv_get_property(
                    self.mpv,
                    name.as_ptr() as *const c_char,
                    FORMAT_DOUBLE,
                    &mut out as *mut f64 as *mut c_void,
                );
            }
            if out.is_finite() {
                out
            } else {
                0.0
            }
        }

        /// Current playback position in seconds (0 if unknown).
        pub fn position(&self) -> f64 {
            self.get_double(b"time-pos\0")
        }
        /// Total duration in seconds (0 if unknown).
        pub fn duration(&self) -> f64 {
            self.get_double(b"duration\0")
        }
        /// Volume (0–100).
        pub fn volume(&self) -> f64 {
            self.get_double(b"volume\0")
        }
        /// Whether playback is paused.
        pub fn paused(&self) -> bool {
            let mut out: c_int = 0;
            unsafe {
                mpv_get_property(
                    self.mpv,
                    b"pause\0".as_ptr() as *const c_char,
                    FORMAT_FLAG,
                    &mut out as *mut c_int as *mut c_void,
                );
            }
            out != 0
        }

        /// Seek to an absolute time in seconds (exact).
        pub fn seek(&self, secs: f64) {
            let s = format!("{secs:.3}");
            self.command(&["seek", &s, "absolute"]);
        }

        fn get_int(&self, name: &[u8]) -> i64 {
            let mut out: i64 = 0;
            unsafe {
                mpv_get_property(
                    self.mpv,
                    name.as_ptr() as *const c_char,
                    FORMAT_INT64,
                    &mut out as *mut i64 as *mut c_void,
                );
            }
            out
        }
        /// Current audio / subtitle track ids (0 = none/off).
        pub fn aid(&self) -> i64 {
            self.get_int(b"aid\0")
        }
        pub fn sid(&self) -> i64 {
            self.get_int(b"sid\0")
        }

        /// A property read by a runtime-built name (e.g. "track-list/3/title").
        fn prop_int(&self, name: &str) -> i64 {
            let Ok(c) = CString::new(name) else { return 0 };
            let mut out: i64 = 0;
            unsafe {
                mpv_get_property(self.mpv, c.as_ptr(), FORMAT_INT64, &mut out as *mut i64 as *mut c_void);
            }
            out
        }
        fn prop_flag(&self, name: &str) -> bool {
            let Ok(c) = CString::new(name) else { return false };
            let mut out: c_int = 0;
            unsafe {
                mpv_get_property(self.mpv, c.as_ptr(), FORMAT_FLAG, &mut out as *mut c_int as *mut c_void);
            }
            out != 0
        }
        fn prop_str(&self, name: &str) -> String {
            let Ok(c) = CString::new(name) else { return String::new() };
            unsafe {
                let p = mpv_get_property_string(self.mpv, c.as_ptr());
                if p.is_null() {
                    return String::new();
                }
                let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
                mpv_free(p as *mut c_void);
                s
            }
        }

        /// All audio + subtitle tracks (for the selection menus), each with a readable label.
        pub fn tracks(&self) -> Vec<super::Track> {
            let count = self.prop_int("track-list/count").max(0);
            // Count audio tracks so a lone one is labelled "Original" rather than its language.
            let audio_total = (0..count)
                .filter(|i| self.prop_str(&format!("track-list/{i}/type")) == "audio")
                .count();
            let mut out = Vec::new();
            for i in 0..count {
                let kind = self.prop_str(&format!("track-list/{i}/type"));
                let audio = kind == "audio";
                if !audio && kind != "sub" {
                    continue; // skip video tracks
                }
                let id = self.prop_int(&format!("track-list/{i}/id"));
                let selected = self.prop_flag(&format!("track-list/{i}/selected"));
                let title = self.prop_str(&format!("track-list/{i}/title"));
                let lang = self.prop_str(&format!("track-list/{i}/lang"));
                let codec = self.prop_str(&format!("track-list/{i}/codec"));
                // Show the track's real name: its own title, else the full language name, else the
                // codec, else a numbered fallback. A lone, untitled audio track is the "Original".
                let lang_name = lang_full_name(&lang);
                let label = if audio && title.is_empty() && audio_total == 1 {
                    "Original".to_string()
                } else {
                    match (title.as_str(), lang_name.as_str(), codec.as_str()) {
                        ("", "", "") => format!("{} {id}", if audio { "Audio" } else { "Subtitle" }),
                        ("", "", c) => c.to_uppercase(),
                        ("", l, _) => l.to_string(),
                        (t, "", _) => t.to_string(),
                        (t, l, _) => format!("{t} ({l})"),
                    }
                };
                out.push(super::Track { id, audio, label, selected });
            }
            out
        }

        pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, disp_w: u32, disp_h: u32) {
            // Pump mpv events so the core keeps progressing.
            unsafe {
                loop {
                    let ev = mpv_wait_event(self.mpv, 0.0);
                    if ev.is_null() || (*ev).event_id == 0 {
                        break;
                    }
                }
            }

            // Match the render surface to the display aspect so mpv letterboxes the clip and lays
            // out subtitles for the real screen. Rebuild the texture + bind group on a size change.
            let (tw, th) = cap_dims(disp_w, disp_h);
            if (tw, th) != (self.tw, self.th) {
                self.tw = tw;
                self.th = th;
                self.buf = vec![0u32; (tw * th) as usize];
                self.tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("video"),
                    size: wgpu::Extent3d {
                        width: tw,
                        height: th,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = self.tex.create_view(&Default::default());
                self.tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.tex_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
            }

            // Render the current frame into our buffer at the surface size.
            let mut size = [self.tw as c_int, self.th as c_int];
            let mut stride: usize = self.tw as usize * 4;
            let mut params = [
                MpvRenderParam {
                    type_: SW_SIZE,
                    data: size.as_mut_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: SW_FORMAT,
                    data: c"rgb0".as_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: SW_STRIDE,
                    data: &mut stride as *mut usize as *mut c_void,
                },
                MpvRenderParam {
                    type_: SW_POINTER,
                    data: self.buf.as_mut_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: INVALID,
                    data: ptr::null_mut(),
                },
            ];
            unsafe {
                mpv_render_context_render(self.render, params.as_mut_ptr());
            }
            // First non-black frame → mpv has decoded the clip (the buffer is rgb0, so any non-zero
            // pixel means content). `any` short-circuits; we only scan fully while still black.
            if !self.has_frame && self.buf.iter().any(|&p| p != 0) {
                self.has_frame = true;
            }
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&self.buf),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(self.tw * 4),
                    rows_per_image: Some(self.th),
                },
                wgpu::Extent3d {
                    width: self.tw,
                    height: self.th,
                    depth_or_array_layers: 1,
                },
            );
        }

        /// Draw the video as a screen-space NDC rect (x, y bottom-left, w, h) at the given fade alpha.
        pub fn draw<'a>(
            &'a self,
            rp: &mut wgpu::RenderPass<'a>,
            rect_ndc: [f32; 4],
            alpha: f32,
            queue: &wgpu::Queue,
        ) {
            queue.write_buffer(
                &self.rect_buf,
                0,
                bytemuck::cast_slice(&[Rect {
                    rect: rect_ndc,
                    alpha: [alpha, 0.0, 0.0, 0.0],
                }]),
            );
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &self.tex_bg, &[]);
            rp.set_bind_group(1, &self.rect_bg, &[]);
            rp.draw(0..6, 0..1);
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            unsafe {
                mpv_render_context_free(self.render);
                mpv_terminate_destroy(self.mpv); // stop playback + audio synchronously
            }
        }
    }

    // The Player is created, used and dropped entirely on the main thread.
    unsafe impl Send for Player {}
}
