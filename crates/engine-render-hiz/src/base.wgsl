struct Viewport { x: u32, y: u32, width: u32, height: u32 }
@group(0) @binding(0) var source: texture_depth_2d;
@group(0) @binding(1) var destination: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> viewport: Viewport;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= viewport.width || id.y >= viewport.height) { return; }
    let value = textureLoad(source, vec2<i32>(id.xy + vec2<u32>(viewport.x, viewport.y)), 0);
    let safe = select(1.0, value, value >= 0.0 && value <= 1.0);
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(safe, 0.0, 0.0, 0.0));
}