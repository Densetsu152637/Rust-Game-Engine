// One deterministic 64-lane workgroup per batch. Blocks and surviving
// instances remain in source order; no atomic append or cross-batch writes.
struct EngineInstance {
    world_from_local: mat4x4<f32>,
    normal_from_local: mat4x4<f32>,
    previous_world_from_local: mat4x4<f32>,
    params: vec4<u32>,
}
struct BatchMeta {
    first: u32,
    count: u32,
    output_base: u32,
    draw_count: u32,
}
struct DrawArgs {
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
}
struct IndexedArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}
@group(0) @binding(0) var<storage, read> masks: array<u32>;
@group(0) @binding(1) var<storage, read> source: array<EngineInstance>;
@group(0) @binding(2) var<storage, read_write> compacted: array<EngineInstance>;
@group(0) @binding(3) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(4) var<storage, read_write> indexed_args: array<IndexedArgs>;
@group(0) @binding(5) var<storage, read> batches: array<BatchMeta>;
var<workgroup> flags: array<u32, 64>;
var<workgroup> accumulated: u32;

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) workgroup: vec3<u32>,
        @builtin(local_invocation_index) lane: u32) {
    let batch = workgroup.x;
    let batch_info = batches[batch];
    if (lane == 0u) { accumulated = 0u; }
    workgroupBarrier();
    for (var block = 0u; block < 1024u; block += 64u) {
        let local = block + lane;
        var visible = false;
        if (local < batch_info.count) { visible = masks[batch_info.first + local] != 0u; }
        flags[lane] = select(0u, 1u, visible);
        workgroupBarrier();
        var prefix = 0u;
        for (var i = 0u; i < lane; i++) {
            prefix += flags[i];
        }
        if (visible) {
            compacted[batch_info.output_base + accumulated + prefix] =
                source[batch_info.first + local];
        }
        workgroupBarrier();
        if (lane == 0u) {
            for (var i = 0u; i < 64u; i++) {
                accumulated += flags[i];
            }
        }
        workgroupBarrier();
    }
    if (lane == 0u) {
        draw_args[batch] = DrawArgs(batch_info.draw_count, accumulated, 0u, 0u);
        indexed_args[batch] = IndexedArgs(batch_info.draw_count, accumulated, 0u, 0, 0u);
    }
}
