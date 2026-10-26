struct Query {
    rect: vec4<i32>, // half-open x0,y0,x1,y1 in base-view pixels
    nearest_depth: f32,
    uncertain: u32,
    padding: vec2<u32>,
}
struct Queries { entries: array<Query> }
struct Results { visible: array<u32> }
struct Params { width: u32, height: u32, count: u32, levels: u32 }
@group(0) @binding(0) var pyramid: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> queries: Queries;
@group(0) @binding(2) var<storage, read_write> results: Results;
@group(0) @binding(3) var<uniform> params: Params;

fn ceil_div(n: u32, d: u32) -> u32 { return (n + d - 1u) / d; }

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index >= params.count) { return; }
    let q = queries.entries[index];
    if (q.uncertain != 0u ||
        !(q.nearest_depth >= 0.0 && q.nearest_depth <= 1.0) ||
        q.rect.x < 0 || q.rect.y < 0 || q.rect.x >= q.rect.z || q.rect.y >= q.rect.w ||
        q.rect.z > i32(params.width) || q.rect.w > i32(params.height)) {
        results.visible[index] = 1u;
        return;
    }
    var x0 = u32(q.rect.x);
    var y0 = u32(q.rect.y);
    var x1 = u32(q.rect.z);
    var y1 = u32(q.rect.w);
    var width = params.width;
    var height = params.height;
    var level = 0u;
    // Each parent texel covers an exact partition of the previous mip.
    // Advance until the query touches at most 4x4 texels.
    loop {
        if ((x1 - x0 <= 4u && y1 - y0 <= 4u) || level + 1u >= params.levels) { break; }
        let next_width = max(1u, width / 2u);
        let next_height = max(1u, height / 2u);
        x0 = ceil_div((x0 + 1u) * next_width, width) - 1u;
        y0 = ceil_div((y0 + 1u) * next_height, height) - 1u;
        x1 = ceil_div(x1 * next_width, width);
        y1 = ceil_div(y1 * next_height, height);
        width = next_width;
        height = next_height;
        level++;
    }
    var greatest = 0.0;
    for (var y = y0; y < y1; y++) {
        for (var x = x0; x < x1; x++) {
            let d = textureLoad(pyramid, vec2<i32>(x, y), i32(level)).x;
            greatest = max(greatest, select(1.0, d, d >= 0.0 && d <= 1.0));
        }
    }
    results.visible[index] = select(0u, 1u, q.nearest_depth <= greatest);
}
