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
