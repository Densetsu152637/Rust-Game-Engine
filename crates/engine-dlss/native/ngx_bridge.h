#pragma once
#include <vulkan/vulkan.h>
#include <cstdint>
// Shared runtime: feature adapters borrow this opaque handle. Only the Rust
// Context owner initializes/shuts down NGX; every feature owns its own params.
struct EngineNgxContext;
extern "C" VkDevice engine_ngx_context_device(const EngineNgxContext* context);
struct Image {
    uint64_t image, view;
    unsigned width, height, format;
};
struct Frame {
    float jitter_x, jitter_y, motion_x, motion_y, delta_ms;
    unsigned reset;
};
// Frame generation (DLSS-G) bridge records. Matrices are NVIDIA float[4][4]
// rows copied verbatim; booleans are 0/1.
struct FgCaps {
    int available, needs_updated_driver, init_result, runtime_missing;
    unsigned min_driver_major, min_driver_minor, multi_frame_max;
};
struct FgFrame {
    float view_to_clip[16], clip_to_view[16], clip_to_lens_clip[16];
    float clip_to_prev_clip[16], prev_clip_to_clip[16];
    float jitter[2], motion_scale[2], pinhole[2];
    float position[3], up[3], right[3], forward[3];
    float near_plane, far_plane, fov, aspect;
    unsigned reset, depth_inverted, camera_motion_included, hdr, orthographic;
    unsigned multi_frame_count, multi_frame_index;
    uint64_t frame_id;
};
// images: backbuffer, depth, motion, HUD-less (view 0 when absent), output.
struct FgResources {
    Image images[5];
    uint64_t disable_buffer;
    unsigned disable_size;
};
