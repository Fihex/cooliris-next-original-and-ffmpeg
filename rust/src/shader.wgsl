// Tile shader for the wall. One instanced quad per photo; the texture for each tile is a layer
// of a texture_2d_array (square thumbnails for now — aspect-correct sizing is a follow-up).

struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var atlas: texture_2d_array<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct VsIn {
    // per-vertex (unit quad)
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    // per-instance (one tile)
    @location(2) offset: vec2<f32>,
    @location(3) size: vec2<f32>,
    @location(4) layer: u32,
    // fraction of the layer the image actually occupies (it's resized to fit, preserving aspect,
    // so a 3:2 photo only fills the top ~0.67 of a square layer).
    @location(5) uv_extent: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // pos is 0..1 → center the quad, scale to the tile size, translate to the tile's spot, place
    // the wall in the z = 0 plane.
    let local = (in.pos - vec2<f32>(0.5, 0.5)) * in.size;
    let world = vec3<f32>(in.offset + local, 0.0);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = in.uv * in.uv_extent; // sample only the used sub-rect of the layer
    out.layer = in.layer;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(atlas, atlas_sampler, in.uv, i32(in.layer));
}
