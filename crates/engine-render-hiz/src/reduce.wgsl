struct Dimensions { source_width: u32, source_height: u32, dest_width: u32, dest_height: u32 }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> dimensions: Dimensions;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= dimensions.dest_width || id.y >= dimensions.dest_height) { return; }
    let x0 = id.x * dimensions.source_width / dimensions.dest_width;
    let x1 = (id.x + 1u) * dimensions.source_width / dimensions.dest_width;
    let y0 = id.y * dimensions.source_height / dimensions.dest_height;
    let y1 = (id.y + 1u) * dimensions.source_height / dimensions.dest_height;
    var greatest = 0.0;
    for (var y = y0; y < y1; y++) {
        for (var x = x0; x < x1; x++) {
            let value = textureLoad(source, vec2<u32>(x, y), 0).x;
            greatest = max(greatest, select(1.0, value, value >= 0.0 && value <= 1.0));
        }
    }
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(greatest, 0.0, 0.0, 0.0));
}