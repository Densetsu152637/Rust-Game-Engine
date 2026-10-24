// This bridge compiles against separately supplied official NVIDIA and Vulkan
// headers. No NVIDIA SDK headers, loader libraries, or runtime binaries are vendored.
#include "ngx_bridge.h"
#include <nvsdk_ngx_vk.h>
#include <nvsdk_ngx_helpers_vk.h>
#include <cstring>
#include <new>

struct Bridge {
    VkDevice device;
    NVSDK_NGX_Parameter* params = nullptr;
    NVSDK_NGX_Handle* feature = nullptr;
    unsigned width = 0, height = 0, output_width = 0, output_height = 0;
};
struct EngineNgxContext { VkDevice device; NVSDK_NGX_Parameter* params = nullptr; };

static bool has_extension(const char* required, const char* const* enabled, unsigned count) {
    for (unsigned i = 0; i < count; ++i) if (std::strcmp(required, enabled[i]) == 0) return true;
    return false;
}

extern "C" {
uint32_t engine_ngx_api_version() { return NVSDK_NGX_Version_API; }
uint32_t engine_ngx_device_extensions(uint64_t instance, uint64_t physical, unsigned feature,
    const char* project, const wchar_t* logs, const wchar_t* plugins,
    char* names, unsigned capacity, unsigned* out_count) {
    const wchar_t* paths[] = {plugins};
    NVSDK_NGX_FeatureCommonInfo common{};
    common.PathListInfo.Path = paths; common.PathListInfo.Length = 1;
    NVSDK_NGX_FeatureDiscoveryInfo discovery{};
    discovery.SDKVersion = NVSDK_NGX_Version_API;
    discovery.FeatureID = static_cast<NVSDK_NGX_Feature>(feature);
    discovery.Identifier.IdentifierType = NVSDK_NGX_Application_Identifier_Type_Project_Id;
    discovery.Identifier.v.ProjectDesc.ProjectId = project;
    discovery.Identifier.v.ProjectDesc.EngineType = NVSDK_NGX_ENGINE_TYPE_CUSTOM;
    discovery.Identifier.v.ProjectDesc.EngineVersion = "0.1.0";
    discovery.ApplicationDataPath = logs; discovery.FeatureInfo = &common;
    VkExtensionProperties* extensions = nullptr;
    unsigned count = 0;
    auto r = NVSDK_NGX_VULKAN_GetFeatureDeviceExtensionRequirements(reinterpret_cast<VkInstance>(instance), reinterpret_cast<VkPhysicalDevice>(physical), &discovery, &count, &extensions);
    if (NVSDK_NGX_FAILED(r)) return r;
    if (count > capacity) return NVSDK_NGX_Result_FAIL_InvalidParameter;
    for (unsigned i = 0; i < count; ++i) {
        std::strncpy(names + i * 256, extensions[i].extensionName, 255);
        names[i * 256 + 255] = 0;
    }
    *out_count = count;
    return 0;
}
// Return 0 only on success; preserve NVIDIA error codes unchanged otherwise.
uint32_t engine_ngx_open(uint64_t instance, uint64_t physical, uint64_t device,
    PFN_vkGetInstanceProcAddr gipa, PFN_vkGetDeviceProcAddr gdpa,
    const char* project, const wchar_t* logs, const wchar_t* plugins,
    const char* const* instance_extensions, unsigned instance_count,
    const char* const* device_extensions, unsigned device_count,
    EngineNgxContext** out, char* missing, unsigned missing_capacity) {
    *out = nullptr;
    const wchar_t* paths[] = {plugins};
    NVSDK_NGX_FeatureCommonInfo common{};
    common.PathListInfo.Path = paths;
    common.PathListInfo.Length = 1;
    NVSDK_NGX_FeatureDiscoveryInfo discovery{};
    discovery.SDKVersion = NVSDK_NGX_Version_API;
    discovery.FeatureID = NVSDK_NGX_Feature_SuperSampling;
    discovery.Identifier.IdentifierType = NVSDK_NGX_Application_Identifier_Type_Project_Id;
    discovery.Identifier.v.ProjectDesc.ProjectId = project;
    discovery.Identifier.v.ProjectDesc.EngineType = NVSDK_NGX_ENGINE_TYPE_CUSTOM;
    discovery.Identifier.v.ProjectDesc.EngineVersion = "0.1.0";
    discovery.ApplicationDataPath = logs;
    discovery.FeatureInfo = &common;
    VkExtensionProperties* extensions = nullptr;
    unsigned count = 0;
    auto result = NVSDK_NGX_VULKAN_GetFeatureInstanceExtensionRequirements(&discovery, &count, &extensions);
    if (NVSDK_NGX_FAILED(result)) return result;
    for (unsigned i = 0; i < count; ++i) {
        if (!has_extension(extensions[i].extensionName, instance_extensions, instance_count)) {
            if (missing_capacity) { std::strncpy(missing, extensions[i].extensionName, missing_capacity - 1); missing[missing_capacity - 1] = 0; }
            return NVSDK_NGX_Result_FAIL_UnsupportedParameter;
        }
    }
    result = NVSDK_NGX_VULKAN_GetFeatureDeviceExtensionRequirements(reinterpret_cast<VkInstance>(instance), reinterpret_cast<VkPhysicalDevice>(physical), &discovery, &count, &extensions);
    if (NVSDK_NGX_FAILED(result)) return result;
    for (unsigned i = 0; i < count; ++i) {
        if (!has_extension(extensions[i].extensionName, device_extensions, device_count)) {
            if (missing_capacity) { std::strncpy(missing, extensions[i].extensionName, missing_capacity - 1); missing[missing_capacity - 1] = 0; }
            return NVSDK_NGX_Result_FAIL_UnsupportedParameter;
        }
    }
    result = NVSDK_NGX_VULKAN_Init_with_ProjectID(project, NVSDK_NGX_ENGINE_TYPE_CUSTOM,
        "0.1.0", logs, reinterpret_cast<VkInstance>(instance), reinterpret_cast<VkPhysicalDevice>(physical),
        reinterpret_cast<VkDevice>(device), gipa, gdpa, &common);
    if (NVSDK_NGX_FAILED(result)) return result;
    EngineNgxContext* bridge = new (std::nothrow) EngineNgxContext{};
    if (!bridge) { NVSDK_NGX_VULKAN_Shutdown1(reinterpret_cast<VkDevice>(device)); return NVSDK_NGX_Result_FAIL_OutOfGPUMemory; }
    bridge->device = reinterpret_cast<VkDevice>(device);
    result = NVSDK_NGX_VULKAN_GetCapabilityParameters(&bridge->params);
    if (NVSDK_NGX_FAILED(result)) { NVSDK_NGX_VULKAN_Shutdown1(bridge->device); delete bridge; return result; }
    int available = 0;
    result = NVSDK_NGX_Parameter_GetI(bridge->params, NVSDK_NGX_Parameter_SuperSampling_Available, &available);
    if (NVSDK_NGX_FAILED(result) || !available) {
        NVSDK_NGX_VULKAN_DestroyParameters(bridge->params);
        NVSDK_NGX_VULKAN_Shutdown1(bridge->device);
        delete bridge;
        return NVSDK_NGX_FAILED(result) ? result : NVSDK_NGX_Result_FAIL_FeatureNotSupported;
    }
    *out = bridge;
    return 0;
}
VkDevice engine_ngx_context_device(const EngineNgxContext* context) { return context->device; }
uint32_t engine_ngx_create_sr(EngineNgxContext* context, Bridge** out) {
    *out = nullptr;
    Bridge* bridge = new (std::nothrow) Bridge{};
    if (!bridge) return NVSDK_NGX_Result_FAIL_OutOfGPUMemory;
    bridge->device = context->device;
    auto result = NVSDK_NGX_VULKAN_GetCapabilityParameters(&bridge->params);
    if (NVSDK_NGX_FAILED(result)) { delete bridge; return result; }
    *out = bridge;
    return 0;
}
void engine_ngx_close_context(EngineNgxContext* context) {
    if (!context) return;
    NVSDK_NGX_VULKAN_DestroyParameters(context->params);
    NVSDK_NGX_VULKAN_Shutdown1(context->device);
    delete context;
}
uint32_t engine_ngx_settings(Bridge* b, unsigned width, unsigned height, int quality, unsigned* render_width, unsigned* render_height, unsigned* driver_major, unsigned* driver_minor) {
    void* callback = nullptr;
    auto r = NVSDK_NGX_Parameter_GetVoidPointer(b->params, NVSDK_NGX_Parameter_DLSSOptimalSettingsCallback, &callback);
    if (NVSDK_NGX_FAILED(r) || !callback) return NVSDK_NGX_Result_FAIL_OutOfDate;
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_Parameter_Width, width);
    NVSDK_NGX_Parameter_SetUI(b->params, NVSDK_NGX_Parameter_Height, height);
    NVSDK_NGX_Parameter_SetI(b->params, NVSDK_NGX_Parameter_PerfQualityValue, quality);
    NVSDK_NGX_Parameter_SetI(b->params, NVSDK_NGX_Parameter_RTXValue, 0);
    r = reinterpret_cast<PFN_NVSDK_NGX_DLSS_GetOptimalSettingsCallback>(callback)(b->params);
    if (NVSDK_NGX_FAILED(r)) return r;
    r = NVSDK_NGX_Parameter_GetUI(b->params, NVSDK_NGX_Parameter_OutWidth, render_width);
    if (NVSDK_NGX_FAILED(r)) return r;
    r = NVSDK_NGX_Parameter_GetUI(b->params, NVSDK_NGX_Parameter_OutHeight, render_height);
    if (NVSDK_NGX_FAILED(r)) return r;
    NVSDK_NGX_Parameter_GetUI(b->params, NVSDK_NGX_Parameter_SuperSampling_MinDriverVersionMajor, driver_major);
    NVSDK_NGX_Parameter_GetUI(b->params, NVSDK_NGX_Parameter_SuperSampling_MinDriverVersionMinor, driver_minor);
    b->width = *render_width; b->height = *render_height;
    b->output_width = width; b->output_height = height;
    return 0;
}
uint32_t engine_ngx_evaluate(Bridge* b, uint64_t command, int quality, const Image* images, const Frame* frame) {
    auto cmd = reinterpret_cast<VkCommandBuffer>(command);
    if (!b->feature) {
        NVSDK_NGX_DLSS_Create_Params create{};
        create.Feature.InWidth = b->width; create.Feature.InHeight = b->height;
        create.Feature.InTargetWidth = b->output_width; create.Feature.InTargetHeight = b->output_height;
        create.Feature.InPerfQualityValue = static_cast<NVSDK_NGX_PerfQuality_Value>(quality);
        create.InFeatureCreateFlags = NVSDK_NGX_DLSS_Feature_Flags_IsHDR | NVSDK_NGX_DLSS_Feature_Flags_MVLowRes | NVSDK_NGX_DLSS_Feature_Flags_AutoExposure;
        auto r = NGX_VULKAN_CREATE_DLSS_EXT1(b->device, cmd, 1, 1, &b->feature, b->params, &create);
        if (NVSDK_NGX_FAILED(r)) return r;
    }
    NVSDK_NGX_Resource_VK resources[4]{};
    for (unsigned i = 0; i < 4; ++i) {
        VkImageSubresourceRange range{};
        range.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT; range.levelCount = 1; range.layerCount = 1;
        resources[i] = NVSDK_NGX_Create_ImageView_Resource_VK(reinterpret_cast<VkImageView>(images[i].view), reinterpret_cast<VkImage>(images[i].image), range, static_cast<VkFormat>(images[i].format), images[i].width, images[i].height, i == 3);
    }
    NVSDK_NGX_VK_DLSS_Eval_Params eval{};
    eval.Feature.pInColor = &resources[0]; eval.pInDepth = &resources[1];
    eval.pInMotionVectors = &resources[2]; eval.Feature.pInOutput = &resources[3];
    eval.InJitterOffsetX = frame->jitter_x; eval.InJitterOffsetY = frame->jitter_y;
    eval.InMVScaleX = frame->motion_x; eval.InMVScaleY = frame->motion_y;
    eval.InReset = frame->reset != 0; eval.InFrameTimeDeltaInMsec = frame->delta_ms;
    eval.InRenderSubrectDimensions.Width = b->width; eval.InRenderSubrectDimensions.Height = b->height;
    auto r = NGX_VULKAN_EVALUATE_DLSS_EXT(cmd, b->feature, b->params, &eval);
    return NVSDK_NGX_FAILED(r) ? r : 0;
}
void engine_ngx_close(Bridge* b) {
    if (!b) return;
    if (b->feature) NVSDK_NGX_VULKAN_ReleaseFeature(b->feature);
    if (b->params) NVSDK_NGX_VULKAN_DestroyParameters(b->params);
    delete b;
}
}
