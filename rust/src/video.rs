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

/// No-op player for builds without the `video` feature — focusing a video tile just shows the
/// placeholder + camera zoom.
#[cfg(not(feature = "video"))]
mod stub {
    pub struct Player;
    impl Player {
        pub fn start(
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _camera_bgl: &wgpu::BindGroupLayout,
            _format: wgpu::TextureFormat,
            _path: &std::path::Path,
        ) -> Player {
            Player
        }
        pub fn update(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue) {}
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
        #[allow(clippy::too_many_arguments)]
        pub fn draw<'a>(
            &'a self,
            _rp: &mut wgpu::RenderPass<'a>,
            _camera_bg: &'a wgpu::BindGroup,
            _offset: [f32; 2],
            _size: [f32; 2],
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

    const VW: usize = 1280;
    const VH: usize = 720;

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
        fn mpv_destroy(ctx: *mut MpvHandle);
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
        offset: [f32; 2],
        size: [f32; 2],
    }

    const SHADER: &str = r#"
struct Camera { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var vid: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
struct Rect { offset: vec2<f32>, size: vec2<f32> };
@group(2) @binding(0) var<uniform> rect: Rect;

struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    var corners = array<vec2<f32>, 6>(
        vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let c = corners[i];
    let local = (c - vec2(0.5, 0.5)) * rect.size;
    let world = vec3(rect.offset + local, 0.01); // just in front of the tile
    var out: V;
    out.clip = camera.view_proj * vec4(world, 1.0);
    out.uv = vec2(c.x, 1.0 - c.y);
    return out;
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    return vec4(textureSample(vid, samp, in.uv).rgb, 1.0);
}
"#;

    pub struct Player {
        mpv: *mut MpvHandle,
        render: *mut MpvRenderContext,
        buf: Vec<u32>,
        tex: wgpu::Texture,
        pipeline: wgpu::RenderPipeline,
        tex_bg: wgpu::BindGroup,
        rect_buf: wgpu::Buffer,
        rect_bg: wgpu::BindGroup,
    }

    impl Player {
        pub fn start(
            device: &wgpu::Device,
            _queue: &wgpu::Queue,
            camera_bgl: &wgpu::BindGroupLayout,
            format: wgpu::TextureFormat,
            path: &Path,
        ) -> Player {
            // mpv core + SW render context.
            let mpv = unsafe {
                let h = mpv_create();
                mpv_set_option_string(h, c"vo".as_ptr(), c"libmpv".as_ptr());
                mpv_set_option_string(h, c"terminal".as_ptr(), c"no".as_ptr());
                mpv_set_option_string(h, c"loop".as_ptr(), c"inf".as_ptr());
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

            // wgpu side: video texture + sampler + pipeline (shares the wall camera at group 0).
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video"),
                size: wgpu::Extent3d {
                    width: VW as u32,
                    height: VH as u32,
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
                    visibility: wgpu::ShaderStages::VERTEX,
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
                bind_group_layouts: &[camera_bgl, &tex_bgl, &rect_bgl],
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
                    targets: &[Some(format.into())],
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
                buf: vec![0u32; VW * VH],
                tex,
                pipeline,
                tex_bg,
                rect_buf,
                rect_bg,
            }
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

        /// Seek to an absolute time in seconds.
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

        pub fn update(&mut self, _device: &wgpu::Device, queue: &wgpu::Queue) {
            // Pump mpv events so the core keeps progressing.
            unsafe {
                loop {
                    let ev = mpv_wait_event(self.mpv, 0.0);
                    if ev.is_null() || (*ev).event_id == 0 {
                        break;
                    }
                }
            }
            // Render the current frame into our buffer.
            let mut size = [VW as c_int, VH as c_int];
            let mut stride: usize = VW * 4;
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
                    bytes_per_row: Some(VW as u32 * 4),
                    rows_per_image: Some(VH as u32),
                },
                wgpu::Extent3d {
                    width: VW as u32,
                    height: VH as u32,
                    depth_or_array_layers: 1,
                },
            );
        }

        /// Draw the video on a quad at `offset` with `size` (world units), via the wall camera.
        #[allow(clippy::too_many_arguments)]
        pub fn draw<'a>(
            &'a self,
            rp: &mut wgpu::RenderPass<'a>,
            camera_bg: &'a wgpu::BindGroup,
            offset: [f32; 2],
            size: [f32; 2],
            queue: &wgpu::Queue,
        ) {
            queue.write_buffer(
                &self.rect_buf,
                0,
                bytemuck::cast_slice(&[Rect { offset, size }]),
            );
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, camera_bg, &[]);
            rp.set_bind_group(1, &self.tex_bg, &[]);
            rp.set_bind_group(2, &self.rect_bg, &[]);
            rp.draw(0..6, 0..1);
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            unsafe {
                mpv_render_context_free(self.render);
                mpv_destroy(self.mpv);
            }
        }
    }

    // The Player is created, used and dropped entirely on the main thread.
    unsafe impl Send for Player {}
}
