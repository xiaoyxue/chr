#pragma once
#include "media.hpp"
#include <dawn/native/DawnNative.h>
#include <dawn/webgpu_cpp.h>
#include <array>
#include <vector>

inline void vkcheck(VkResult result, const char* action) {
    if (result != VK_SUCCESS) throw std::runtime_error(std::string(action) + ": Vulkan " + std::to_string(result));
}

struct SharedImage {
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkImageCreateInfo create{};
    wgpu::SharedTextureMemory shared;
    wgpu::Texture texture;
    wgpu::TextureView view;
    VkImageLayout layout = VK_IMAGE_LAYOUT_UNDEFINED;
    bool initialized = false;
};

class Bridge {
    AVHWDeviceContext* avdevice;
    AVVulkanDeviceContext* context;
    VkDevice device;
    uint32_t family;
    VkQueue queue;
    VkCommandPool pool;
    VkCommandBuffer command;
    VkBuffer packed;
    VkDeviceMemory packedMemory;
    uint32_t memoryType(uint32_t bits);
    void start();
    void submit(AVVkFrame* frame = nullptr, const std::vector<VkSemaphore>& waits = {});
    void barrier(VkImage image, VkImageLayout oldLayout, VkImageLayout newLayout,
                 VkAccessFlags src, VkAccessFlags dst, uint32_t sourceFamily = VK_QUEUE_FAMILY_IGNORED,
                 uint32_t targetFamily = VK_QUEUE_FAMILY_IGNORED);
    SharedImage makeImage(uint32_t w, uint32_t h, VkFormat format, wgpu::TextureFormat dawnFormat, bool storage);
    std::vector<VkSemaphore> endDawn();
    void beginDawn();
public:
    dawn::native::Instance instance;
    wgpu::Device dawnDevice;
    std::array<SharedImage, 3> images;
    uint32_t width, height;
    Bridge(AVBufferRef* avDevice, uint32_t w, uint32_t h);
    ~Bridge();
    void receive(AVFrame* input);
    void deliver(AVFrame* output);
};
