// SVG UI icons, baked into a small atlas at startup and drawn as tinted, instanced quads.
//
// The icons are the reference website's own inline SVG paths (saved in assets/icons/*.svg and
// embedded here at build time), so the native video controls / lightbox arrows match the site.
// Each SVG is rasterised (resvg) into a cell of a square atlas; we sample the atlas *alpha* as a
// coverage mask and apply a per-icon tint, so any icon can be drawn in any colour.

use std::collections::HashMap;

/// One icon to draw this frame: a pixel rect (top-left origin), the icon name, and an RGBA tint.
#[derive(Clone, Copy)]
pub struct IconReq {
    pub rect: [f32; 4], // x, y (top-left), w, h — in pixels
    pub name: &'static str,
    pub tint: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct IconInst {
    rect: [f32; 4], // NDC x, y (bottom-left), w, h
    uv: [f32; 4],   // u0, v0 (top), u1, v1 (bottom) within the atlas
    tint: [f32; 4],
}

const ICON_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];

const ICON_SHADER: &str = r#"
struct In { @location(0) rect: vec4<f32>, @location(1) uv: vec4<f32>, @location(2) tint: vec4<f32> };
struct V { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) tint: vec4<f32> };
@vertex
fn vs(@builtin(vertex_index) vi: u32, in: In) -> V {
    var c = array<vec2<f32>, 6>(
        vec2(0.,0.), vec2(1.,0.), vec2(0.,1.), vec2(0.,1.), vec2(1.,0.), vec2(1.,1.));
    let q = c[vi];
    let p = in.rect.xy + q * in.rect.zw;
    var out: V;
    out.clip = vec4(p, 0.0, 1.0);
    // q.y = 0 is the rect bottom (NDC up) → the icon cell's bottom (uv.w); q.y = 1 → top (uv.y).
    out.uv = vec2(mix(in.uv.x, in.uv.z, q.x), mix(in.uv.w, in.uv.y, q.y));
    out.tint = in.tint;
    return out;
}
@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
// sRGB→linear (the swapchain re-encodes), so tints render at their authored hex value.
fn s2l(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}
@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    let a = textureSample(atlas, samp, in.uv).a; // alpha = icon coverage
    return vec4<f32>(s2l(in.tint.rgb), in.tint.a * a);
}
"#;

// Icon name → SVG source (the files live in assets/icons/; baked into the binary).
const SVGS: &[(&str, &str)] = &[
    ("play", include_str!("../assets/icons/play.svg")),
    ("pause", include_str!("../assets/icons/pause.svg")),
    ("back10", include_str!("../assets/icons/back10.svg")),
    ("fwd10", include_str!("../assets/icons/fwd10.svg")),
    ("volume", include_str!("../assets/icons/volume.svg")),
    ("mute", include_str!("../assets/icons/mute.svg")),
    ("fullscreen", include_str!("../assets/icons/fullscreen.svg")),
    ("fullscreen-exit", include_str!("../assets/icons/fullscreen-exit.svg")),
    ("prev", include_str!("../assets/icons/prev.svg")),
    ("next", include_str!("../assets/icons/next.svg")),
    ("info", include_str!("../assets/icons/info.svg")),
    ("back", include_str!("../assets/icons/back.svg")),
    ("cc", include_str!("../assets/icons/cc.svg")),
    ("audio", include_str!("../assets/icons/audio.svg")),
];

const CELL: u32 = 64; // px per icon cell in the atlas
const PAD: f32 = 12.0; // transparent margin within a cell (avoids neighbour bleed when filtered)
const CAP: u64 = 64; // max icons drawn per frame

pub struct Icons {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    inst: wgpu::Buffer,
    uv: HashMap<&'static str, [f32; 4]>,
}

impl Icons {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // Pack the icons into a square grid, rasterising each SVG centred in its cell.
        let n = SVGS.len() as u32;
        let grid = (n as f32).sqrt().ceil() as u32;
        let dim = grid * CELL; // atlas is dim×dim; dim is a multiple of 64 → bytes_per_row 256-aligned
        let mut rgba = vec![0u8; (dim * dim * 4) as usize];
        let mut uv = HashMap::new();
        let opt = resvg::usvg::Options::default();
        for (i, (name, svg)) in SVGS.iter().enumerate() {
            let i = i as u32;
            let (gx, gy) = (i % grid, i / grid);
            if let Ok(tree) = resvg::usvg::Tree::from_str(svg, &opt) {
                let mut pm = resvg::tiny_skia::Pixmap::new(CELL, CELL).unwrap();
                let size = tree.size();
                let s = ((CELL as f32 - PAD) / size.width()).min((CELL as f32 - PAD) / size.height());
                let tx = (CELL as f32 - size.width() * s) * 0.5;
                let ty = (CELL as f32 - size.height() * s) * 0.5;
                let ts = resvg::tiny_skia::Transform::from_scale(s, s).post_translate(tx, ty);
                resvg::render(&tree, ts, &mut pm.as_mut());
                let (ox, oy) = (gx * CELL, gy * CELL);
                let src = pm.data();
                for row in 0..CELL {
                    let s0 = (row * CELL * 4) as usize;
                    let d0 = (((oy + row) * dim + ox) * 4) as usize;
                    rgba[d0..d0 + (CELL * 4) as usize].copy_from_slice(&src[s0..s0 + (CELL * 4) as usize]);
                }
            } else {
                log::warn!("icon {name}: SVG parse failed");
            }
            let c = CELL as f32 / dim as f32;
            uv.insert(*name, [gx as f32 * c, gy as f32 * c, gx as f32 * c + c, gy as f32 * c + c]);
        }

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("icon-atlas"),
            size: wgpu::Extent3d { width: dim, height: dim, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * dim),
                rows_per_image: Some(dim),
            },
            wgpu::Extent3d { width: dim, height: dim, depth_or_array_layers: 1 },
        );
        let view = tex.create_view(&Default::default());
        let samp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("icon-samp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("icon-bgl"),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("icon-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&samp) },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("icon-shader"),
            source: wgpu::ShaderSource::Wgsl(ICON_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("icon-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("icon-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<IconInst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ICON_ATTRS,
                }],
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
        let inst = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("icon-inst"),
            size: CAP * size_of::<IconInst>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, bind_group, inst, uv }
    }

    /// Draw the requested icons (no-op if empty). Call inside the present pass, after the overlay.
    pub fn draw<'a>(
        &'a self,
        rp: &mut wgpu::RenderPass<'a>,
        queue: &wgpu::Queue,
        w: f32,
        h: f32,
        reqs: &[IconReq],
    ) {
        let (w, h) = (w.max(1.0), h.max(1.0));
        let mut insts: Vec<IconInst> = Vec::with_capacity(reqs.len());
        for r in reqs {
            let Some(uv) = self.uv.get(r.name) else { continue };
            let p = r.rect;
            insts.push(IconInst {
                rect: [p[0] / w * 2.0 - 1.0, 1.0 - (p[1] + p[3]) / h * 2.0, p[2] / w * 2.0, p[3] / h * 2.0],
                uv: *uv,
                tint: [
                    r.tint[0] as f32 / 255.0,
                    r.tint[1] as f32 / 255.0,
                    r.tint[2] as f32 / 255.0,
                    r.tint[3] as f32 / 255.0,
                ],
            });
            if insts.len() >= CAP as usize {
                break;
            }
        }
        if insts.is_empty() {
            return;
        }
        queue.write_buffer(&self.inst, 0, bytemuck::cast_slice(&insts));
        rp.set_pipeline(&self.pipeline);
        rp.set_bind_group(0, &self.bind_group, &[]);
        rp.set_vertex_buffer(0, self.inst.slice(..));
        rp.draw(0..6, 0..insts.len() as u32);
    }
}
