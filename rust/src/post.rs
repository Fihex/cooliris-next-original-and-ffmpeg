// Post-processing for the lightbox backdrop: the wall is rendered to an offscreen texture, blurred
// (a two-pass separable gaussian at half resolution), then composited to the swapchain. When an
// item is focused the composite cross-fades the sharp wall toward the dark, blurred version — so
// the focused photo/video sits on a blurred, dimmed background instead of a flat dark overlay.
//
// Flow each frame (driven by State::render):
//   1. wall (tiles)        → scene_tex          [scene pass]
//   2. scene_tex (H blur)  → blur_a (half res)  [only when focused]
//   3. blur_a   (V blur)   → blur_b (half res)  [only when focused]
//   4. composite(scene, blur_b) → swapchain, mixed + darkened by focus     [swapchain pass]

const BLUR_RADIUS: f32 = 1.5; // gaussian step in half-res pixels (wider = blurrier)

const BLUR_SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> dir: vec4<f32>; // texel step (xy) in UV; zw unused

struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    var p = array<vec2<f32>, 3>(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    let c = p[i];
    var o: V;
    o.clip = vec4(c, 0., 1.);
    o.uv = vec2(c.x, -c.y) * 0.5 + 0.5;
    return o;
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    var w = array<f32, 5>(0.227027, 0.194595, 0.121622, 0.054054, 0.016216);
    var col = textureSample(src, samp, in.uv).rgb * w[0];
    for (var k = 1; k < 5; k = k + 1) {
        let off = dir.xy * f32(k);
        col = col + textureSample(src, samp, in.uv + off).rgb * w[k];
        col = col + textureSample(src, samp, in.uv - off).rgb * w[k];
    }
    return vec4(col, 1.0);
}
"#;

const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var blurred: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: vec4<f32>; // mix, dark, unused, unused

struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    var p = array<vec2<f32>, 3>(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    let c = p[i];
    var o: V;
    o.clip = vec4(c, 0., 1.);
    o.uv = vec2(c.x, -c.y) * 0.5 + 0.5;
    return o;
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    let sharp = textureSample(scene, samp, in.uv).rgb;
    let blur = textureSample(blurred, samp, in.uv).rgb;
    let bg = mix(sharp, blur * params.y, params.x);
    return vec4(bg, 1.0);
}
"#;

pub struct Post {
    format: wgpu::TextureFormat,
    sampler: wgpu::Sampler,
    blur_pipeline: wgpu::RenderPipeline,
    blur_bgl: wgpu::BindGroupLayout,
    blur_h_buf: wgpu::Buffer,
    blur_v_buf: wgpu::Buffer,
    comp_pipeline: wgpu::RenderPipeline,
    comp_bgl: wgpu::BindGroupLayout,
    comp_buf: wgpu::Buffer,

    // Size-dependent (rebuilt on resize).
    scene_tex: wgpu::Texture,
    blur_a_tex: wgpu::Texture,
    blur_b_tex: wgpu::Texture,
    scene_view: wgpu::TextureView,
    blur_a_view: wgpu::TextureView,
    blur_b_view: wgpu::TextureView,
    blur_h_bg: wgpu::BindGroup,
    blur_v_bg: wgpu::BindGroup,
    comp_bg: wgpu::BindGroup,
}

impl Post {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Post {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // Blur: { src texture, sampler, dir uniform }.
        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur-bgl"),
            entries: &[
                tex_entry(0),
                samp_entry(1),
                uniform_entry(2),
            ],
        });
        let blur_pipeline = fullscreen_pipeline(device, &blur_bgl, BLUR_SHADER, format, "blur");
        let blur_h_buf = uniform_buf(device, "blur-h");
        let blur_v_buf = uniform_buf(device, "blur-v");

        // Composite: { scene, blurred, sampler, params uniform }.
        let comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp-bgl"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                samp_entry(2),
                uniform_entry(3),
            ],
        });
        let comp_pipeline =
            fullscreen_pipeline(device, &comp_bgl, COMPOSITE_SHADER, format, "composite");
        let comp_buf = uniform_buf(device, "comp-params");

        let (scene_tex, blur_a_tex, blur_b_tex) = make_targets(device, format, width, height);
        let scene_view = scene_tex.create_view(&Default::default());
        let blur_a_view = blur_a_tex.create_view(&Default::default());
        let blur_b_view = blur_b_tex.create_view(&Default::default());
        let blur_h_bg =
            blur_bg(device, &blur_bgl, &scene_view, &sampler, &blur_h_buf);
        let blur_v_bg =
            blur_bg(device, &blur_bgl, &blur_a_view, &sampler, &blur_v_buf);
        let comp_bg = comp_bg(
            device, &comp_bgl, &scene_view, &blur_b_view, &sampler, &comp_buf,
        );

        let post = Post {
            format,
            sampler,
            blur_pipeline,
            blur_bgl,
            blur_h_buf,
            blur_v_buf,
            comp_pipeline,
            comp_bgl,
            comp_buf,
            scene_tex,
            blur_a_tex,
            blur_b_tex,
            scene_view,
            blur_a_view,
            blur_b_view,
            blur_h_bg,
            blur_v_bg,
            comp_bg,
        };
        post.write_blur_dirs(queue, width, height);
        post
    }

    pub fn resize(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) {
        let (scene_tex, blur_a_tex, blur_b_tex) = make_targets(device, self.format, width, height);
        self.scene_view = scene_tex.create_view(&Default::default());
        self.blur_a_view = blur_a_tex.create_view(&Default::default());
        self.blur_b_view = blur_b_tex.create_view(&Default::default());
        self.scene_tex = scene_tex;
        self.blur_a_tex = blur_a_tex;
        self.blur_b_tex = blur_b_tex;
        self.blur_h_bg = blur_bg(
            device, &self.blur_bgl, &self.scene_view, &self.sampler, &self.blur_h_buf,
        );
        self.blur_v_bg = blur_bg(
            device, &self.blur_bgl, &self.blur_a_view, &self.sampler, &self.blur_v_buf,
        );
        self.comp_bg = comp_bg(
            device, &self.comp_bgl, &self.scene_view, &self.blur_b_view, &self.sampler,
            &self.comp_buf,
        );
        self.write_blur_dirs(queue, width, height);
    }

    fn write_blur_dirs(&self, queue: &wgpu::Queue, width: u32, height: u32) {
        let hw = (width / 2).max(1) as f32;
        let hh = (height / 2).max(1) as f32;
        let h: [f32; 4] = [BLUR_RADIUS / hw, 0.0, 0.0, 0.0];
        let v: [f32; 4] = [0.0, BLUR_RADIUS / hh, 0.0, 0.0];
        queue.write_buffer(&self.blur_h_buf, 0, bytemuck::cast_slice(&h));
        queue.write_buffer(&self.blur_v_buf, 0, bytemuck::cast_slice(&v));
    }

    /// The offscreen view the wall (tiles) renders into.
    pub fn scene_view(&self) -> &wgpu::TextureView {
        &self.scene_view
    }

    /// `mix` 0 = sharp wall, 1 = fully blurred+dark; `dark` scales the blurred backdrop's brightness.
    pub fn set_params(&self, queue: &wgpu::Queue, mix: f32, dark: f32) {
        let p: [f32; 4] = [mix, dark, 0.0, 0.0];
        queue.write_buffer(&self.comp_buf, 0, bytemuck::cast_slice(&p));
    }

    /// Record the two half-res blur passes (scene → blur_a → blur_b).
    pub fn record_blur(&self, enc: &mut wgpu::CommandEncoder) {
        for (target, bg) in [(&self.blur_a_view, &self.blur_h_bg), (&self.blur_b_view, &self.blur_v_bg)] {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            rp.set_pipeline(&self.blur_pipeline);
            rp.set_bind_group(0, bg, &[]);
            rp.draw(0..3, 0..1);
        }
    }

    /// Draw the composited background (sharp wall ↔ blurred+dark) as a fullscreen triangle.
    pub fn draw_composite<'a>(&'a self, rp: &mut wgpu::RenderPass<'a>) {
        rp.set_pipeline(&self.comp_pipeline);
        rp.set_bind_group(0, &self.comp_bg, &[]);
        rp.draw(0..3, 0..1);
    }
}

fn make_targets(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::Texture, wgpu::Texture) {
    let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    let mk = |w: u32, h: u32, label| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let scene = mk(width, height, "scene");
    let blur_a = mk(width / 2, height / 2, "blur-a");
    let blur_b = mk(width / 2, height / 2, "blur-b");
    (scene, blur_a, blur_b)
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn samp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_buf(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    shader_src: &str,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
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
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

fn blur_bg(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    src: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    dir: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blur-bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: dir.as_entire_binding(),
            },
        ],
    })
}

fn comp_bg(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    scene: &wgpu::TextureView,
    blurred: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    params: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("comp-bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(scene),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(blurred),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params.as_entire_binding(),
            },
        ],
    })
}
