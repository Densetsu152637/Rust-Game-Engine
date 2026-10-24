// DLSS Frame Generation bridge. Compiles against separately supplied official
// NVIDIA DLSS SDK and Vulkan headers; the proprietary structures and parameter
// names come only from those headers. Nothing NVIDIA-owned is vendored.
#include "ngx_bridge.h"
#include <nvsdk_ngx_vk.h>
#include <nvsdk_ngx_helpers_vk.h>
#include <nvsdk_ngx_helpers_dlssg_vk.h>
#include <cfloat>
#include <cstring>
#include <new>

struct FgBridge {
    VkDevice device;
    NVSDK_NGX_Parameter* params = nullptr;
    NVSDK_NGX_Handle* feature = nullptr;
    unsigned width = 0, height = 0, render_width = 0, render_height = 0, format = 0;
    unsigned hudless = 0;
};

static void copy_matrix(float (&out)[4][4], const float* in) {
    std::memcpy(out, in, sizeof(float) * 16);
}
static NVSDK_NGX_Resource_VK image_resource(const Image& image, bool write) {
    VkImageSubresourceRange range{};
    range.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
    range.levelCount = 1;
    range.layerCount = 1;
    return NVSDK_NGX_Create_ImageView_Resource_VK(reinterpret_cast<VkImageView>(image.view),
        reinterpret_cast<VkImage>(image.image), range, static_cast<VkFormat>(image.format),
        image.width, image.height, write);
}

// The VK create helper (unlike the D3D12 one) does not forward the render
// size; set it as NGX_D3D12_CREATE_DLSSG does. Dynamic resolution is disabled.
static void set_render_size(const FgBridge* b) {
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_DLSSG_Parameter_InternalWidth, b->render_width);
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_DLSSG_Parameter_InternalHeight, b->render_height);
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_DLSSG_Parameter_DynamicResolution, 0);
}

extern "C" {
// Return 0 only on success; NVIDIA error codes are preserved unchanged.
// Capabilities are filled whenever the capability map could be read so the
// caller can report the driver's actual reason for an unavailable feature.
uint32_t engine_ngx_create_fg(EngineNgxContext* context, unsigned width, unsigned height,
    unsigned render_width, unsigned render_height, unsigned format, unsigned hudless,
    FgBridge** out, FgCaps* caps) {
    *out = nullptr;
    *caps = FgCaps{};
    FgBridge* bridge = new (std::nothrow) FgBridge{};
    if (!bridge) return NVSDK_NGX_Result_FAIL_OutOfGPUMemory;
    bridge->device = engine_ngx_context_device(context);
    auto r = NVSDK_NGX_VULKAN_GetCapabilityParameters(&bridge->params);
    if (NVSDK_NGX_FAILED(r)) { delete bridge; return r; }
    NVSDK_NGX_Parameter_GetI(bridge->params, NVSDK_NGX_Parameter_FrameGeneration_NeedsUpdatedDriver, &caps->needs_updated_driver);
    NVSDK_NGX_Parameter_GetUI(bridge->params, NVSDK_NGX_Parameter_FrameGeneration_MinDriverVersionMajor, &caps->min_driver_major);
    NVSDK_NGX_Parameter_GetUI(bridge->params, NVSDK_NGX_Parameter_FrameGeneration_MinDriverVersionMinor, &caps->min_driver_minor);
    NVSDK_NGX_Parameter_GetI(bridge->params, NVSDK_NGX_Parameter_FrameGeneration_FeatureInitResult, &caps->init_result);
    caps->runtime_missing = static_cast<uint32_t>(caps->init_result) == NVSDK_NGX_Result_FAIL_FeatureNotFound;
    if (NVSDK_NGX_FAILED(NVSDK_NGX_Parameter_GetUI(bridge->params, NVSDK_NGX_DLSSG_Parameter_MultiFrameCountMax, &caps->multi_frame_max)) || caps->multi_frame_max == 0)
        caps->multi_frame_max = 1;  // Header: "If 1 or not set, multiframe is not supported".
    r = NVSDK_NGX_Parameter_GetI(bridge->params, NVSDK_NGX_Parameter_FrameGeneration_Available, &caps->available);
    if (NVSDK_NGX_FAILED(r) || !caps->available) {
        caps->available = 0;
        NVSDK_NGX_VULKAN_DestroyParameters(bridge->params);
        delete bridge;
        return NVSDK_NGX_FAILED(r) ? r : NVSDK_NGX_Result_FAIL_FeatureNotSupported;
    }
    bridge->width = width; bridge->height = height;
    bridge->render_width = render_width; bridge->render_height = render_height;
    bridge->format = format; bridge->hudless = hudless;
    *out = bridge;
    return 0;
}

struct FgViews {
    NVSDK_NGX_Resource_VK backbuffer, depth, motion, hudless, output, disable;
};
static FgViews views(const FgResources* resources) {
    const Image* images = resources->images;
    return FgViews{image_resource(images[0], false), image_resource(images[1], false),
        image_resource(images[2], false), image_resource(images[3], false),
        image_resource(images[4], true),
        NVSDK_NGX_Create_Buffer_Resource_VK(reinterpret_cast<VkBuffer>(resources->disable_buffer),
            resources->disable_size, true)};
}

// Records feature creation into `command`. The caller submits and waits for
// it before the first evaluation and releases the bridge after GPU idle.
uint32_t engine_ngx_create_fg_feature(FgBridge* b, uint64_t command) {
    if (b->feature) return NVSDK_NGX_Result_FAIL_FeatureAlreadyExists;
    auto cmd = reinterpret_cast<VkCommandBuffer>(command);
    unsigned never = NVSDK_NGX_DLSSG_ResourceFlags_UI | NVSDK_NGX_DLSSG_ResourceFlags_UIAlpha
        | NVSDK_NGX_DLSSG_ResourceFlags_BidirectionalDistortionField;
    unsigned always = NVSDK_NGX_DLSSG_ResourceFlags_OutputDisableInterpolation;
    if (b->hudless) always |= NVSDK_NGX_DLSSG_ResourceFlags_HUDLess;
    else never |= NVSDK_NGX_DLSSG_ResourceFlags_HUDLess;
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_DLSSG_Parameter_ResourceAlwaysProvided_Flags, always);
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_DLSSG_Parameter_ResourceNeverProvided_Flags, never);
    set_render_size(b);
    NVSDK_NGX_DLSSG_Create_Params create{};
    create.Width = b->width; create.Height = b->height;
    create.NativeBackbufferFormat = b->format;
    create.RenderWidth = b->render_width; create.RenderHeight = b->render_height;
    create.DynamicResolutionScaling = false;
    auto r = NGX_VK_CREATE_DLSSG(cmd, 1, 1, &b->feature, b->params, &create);
    if (NVSDK_NGX_FAILED(r)) { b->feature = nullptr; return r; }
    return 0;
}

// Records one evaluation into `command`. The caller owns all resources, keeps
// them alive through GPU completion, and submits/waits; NGX does not begin or
// end the command buffer.
uint32_t engine_ngx_evaluate_fg(FgBridge* b, uint64_t command, const FgResources* resources, const FgFrame* frame) {
    if (!b->feature) return NVSDK_NGX_Result_FAIL_NotInitialized;
    auto cmd = reinterpret_cast<VkCommandBuffer>(command);
    FgViews v = views(resources);
    NVSDK_NGX_VK_DLSSG_Eval_Params eval{};
    eval.pBackbuffer = &v.backbuffer;
    eval.pDepth = &v.depth;
    eval.pMVecs = &v.motion;
    eval.pHudless = b->hudless ? &v.hudless : nullptr;
    eval.pOutputInterpFrame = &v.output;
    eval.pOutputDisableInterpolation = &v.disable;

    NVSDK_NGX_DLSSG_Opt_Eval_Params opt{};
    opt.multiFrameCount = frame->multi_frame_count;
    opt.multiFrameIndex = frame->multi_frame_index;
    copy_matrix(opt.cameraViewToClip, frame->view_to_clip);
    copy_matrix(opt.clipToCameraView, frame->clip_to_view);
    copy_matrix(opt.clipToLensClip, frame->clip_to_lens_clip);
    copy_matrix(opt.clipToPrevClip, frame->clip_to_prev_clip);
    copy_matrix(opt.prevClipToClip, frame->prev_clip_to_clip);
    for (int i = 0; i < 2; ++i) {
        opt.jitterOffset[i] = frame->jitter[i];
        opt.mvecScale[i] = frame->motion_scale[i];
        opt.cameraPinholeOffset[i] = frame->pinhole[i];
    }
    for (int i = 0; i < 3; ++i) {
        opt.cameraPos[i] = frame->position[i];
        opt.cameraUp[i] = frame->up[i];
        opt.cameraRight[i] = frame->right[i];
        opt.cameraFwd[i] = frame->forward[i];
    }
    opt.cameraNear = frame->near_plane;
    opt.cameraFar = frame->far_plane;
    opt.cameraFOV = frame->fov;
    opt.cameraAspectRatio = frame->aspect;
    opt.colorBuffersHDR = frame->hdr != 0;
    opt.depthInverted = frame->depth_inverted != 0;
    opt.cameraMotionIncluded = frame->camera_motion_included != 0;
    opt.reset = frame->reset != 0;
    opt.orthoProjection = frame->orthographic != 0;
    opt.motionVectorsDilated = false;
    // The guide gives no default; the struct leaves 0, which would flag every
    // static pixel invalid. FLT_MAX is not representable in the FP16 motion
    // texture, so no stored motion is treated as invalid.
    opt.motionVectorsInvalidValue = FLT_MAX;
    opt.menuDetectionEnabled = false;
    opt.mvecsSubrectSize = {b->render_width, b->render_height};
    opt.depthSubrectSize = {b->render_width, b->render_height};
    if (b->hudless) opt.hudLessSubrectSize = {b->width, b->height};
    opt.backbufferSubrectSize = {b->width, b->height};
    opt.outputInterpSubrectSize = {b->width, b->height};
    // Defined by nvsdk_ngx_defs_dlssg.h but not set by the official helper.
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_DLSSG_Parameter_MvecJittered, 0);
    NVSDK_NGX_Parameter_SetULL(b->params, NVSDK_NGX_DLSSG_Parameter_BackbufferFrameID, frame->frame_id);
    auto r = NGX_VK_EVALUATE_DLSSG(cmd, b->feature, b->params, &eval, &opt);
    return NVSDK_NGX_FAILED(r) ? r : 0;
}

static bool fg_has_extension(const char* required, const char* const* enabled, unsigned count) {
    for (unsigned i = 0; i < count; ++i) if (std::strcmp(required, enabled[i]) == 0) return true;
    return false;
}

// Compare NGX's FG Vulkan instance/device extension requirements with the
// extensions enabled on the context device. On a missing extension, writes its
// terminated name to `missing` and returns FAIL_UnsupportedParameter; other
// NVIDIA error codes are preserved unchanged.
uint32_t engine_ngx_fg_missing_extension(uint64_t instance, uint64_t physical,
    const char* project, const wchar_t* logs, const wchar_t* plugins,
    const char* const* instance_extensions, unsigned instance_count,
    const char* const* device_extensions, unsigned device_count,
    char* missing, unsigned missing_capacity) {
    if (missing_capacity) missing[0] = 0;
    const wchar_t* paths[] = {plugins};
    NVSDK_NGX_FeatureCommonInfo common{};
    common.PathListInfo.Path = paths;
    common.PathListInfo.Length = 1;
    NVSDK_NGX_FeatureDiscoveryInfo discovery{};
    discovery.SDKVersion = NVSDK_NGX_Version_API;
    discovery.FeatureID = NVSDK_NGX_Feature_FrameGeneration;
    discovery.Identifier.IdentifierType = NVSDK_NGX_Application_Identifier_Type_Project_Id;
    discovery.Identifier.v.ProjectDesc.ProjectId = project;
    discovery.Identifier.v.ProjectDesc.EngineType = NVSDK_NGX_ENGINE_TYPE_CUSTOM;
    discovery.Identifier.v.ProjectDesc.EngineVersion = "0.1.0";
    discovery.ApplicationDataPath = logs;
    discovery.FeatureInfo = &common;
    auto report = [&](const char* name) {
        if (missing_capacity) {
            std::strncpy(missing, name, missing_capacity - 1);
            missing[missing_capacity - 1] = 0;
        }
        return static_cast<uint32_t>(NVSDK_NGX_Result_FAIL_UnsupportedParameter);
    };
    VkExtensionProperties* extensions = nullptr;
    unsigned count = 0;
    auto r = NVSDK_NGX_VULKAN_GetFeatureInstanceExtensionRequirements(&discovery, &count, &extensions);
    if (NVSDK_NGX_FAILED(r)) return r;
    for (unsigned i = 0; i < count; ++i)
        if (!fg_has_extension(extensions[i].extensionName, instance_extensions, instance_count))
            return report(extensions[i].extensionName);
    r = NVSDK_NGX_VULKAN_GetFeatureDeviceExtensionRequirements(reinterpret_cast<VkInstance>(instance),
        reinterpret_cast<VkPhysicalDevice>(physical), &discovery, &count, &extensions);
    if (NVSDK_NGX_FAILED(r)) return r;
    for (unsigned i = 0; i < count; ++i)
        if (!fg_has_extension(extensions[i].extensionName, device_extensions, device_count))
            return report(extensions[i].extensionName);
    return 0;
}

// The capability map may only report multi-frame support once a feature
// exists; re-read DLSSG.MultiFrameCountMax from this feature's parameters.
unsigned engine_ngx_fg_multi_frame_max(FgBridge* b) {
    unsigned value = 0;
    if (NVSDK_NGX_FAILED(NVSDK_NGX_Parameter_GetUI(b->params, NVSDK_NGX_DLSSG_Parameter_MultiFrameCountMax, &value)) || value == 0)
        return 1;
    return value;
}
void engine_ngx_close_fg(FgBridge* b) {
    if (!b) return;
    if (b->feature) NVSDK_NGX_VULKAN_ReleaseFeature(b->feature);
    if (b->params) NVSDK_NGX_VULKAN_DestroyParameters(b->params);
    delete b;
}
}
