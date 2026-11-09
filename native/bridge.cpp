#include "bridge.hpp"
#include <dawn/native/VulkanBackend.h>
#include <cstring>
#include <iostream>
#include <unistd.h>

namespace {
class FrameLock {
    AVHWFramesContext* frames;
    AVVulkanFramesContext* vulkan;
    AVVkFrame* frame;
public:
    FrameLock(AVHWFramesContext* fc, AVVkFrame* f)
        : frames(fc), vulkan(static_cast<AVVulkanFramesContext*>(fc->hwctx)), frame(f) {
        vulkan->lock_frame(frames, frame);
    }
    ~FrameLock() { vulkan->unlock_frame(frames, frame); }
};
}

uint32_t Bridge::memoryType(uint32_t bits) {
    VkPhysicalDeviceMemoryProperties props;
    vkGetPhysicalDeviceMemoryProperties(context->phys_dev, &props);
    for (uint32_t i = 0; i < props.memoryTypeCount; ++i)
        if ((bits & (1u << i)) && (props.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) return i;
    throw std::runtime_error("No GPU-local allocation type");
}

Bridge::Bridge(AVBufferRef* avDevice, uint32_t w, uint32_t h) : width(w), height(h) {
    avdevice = reinterpret_cast<AVHWDeviceContext*>(avDevice->data);
    context = reinterpret_cast<AVVulkanDeviceContext*>(avdevice->hwctx);
    device = context->act_dev;
    family = UINT32_MAX;
    for (int i = 0; i < context->nb_qf; ++i)
        if (context->qf[i].flags & VK_QUEUE_TRANSFER_BIT) { family = context->qf[i].idx; break; }
    if (family == UINT32_MAX) throw std::runtime_error("No enabled transfer queue");
    vkGetDeviceQueue(device, family, 0, &queue);
    VkCommandPoolCreateInfo pci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
    pci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pci.queueFamilyIndex = family;
    vkcheck(vkCreateCommandPool(device, &pci, nullptr, &pool), "Create transfer pool");
    VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
    cai.commandPool = pool;
    cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    cai.commandBufferCount = 1;
    vkcheck(vkAllocateCommandBuffers(device, &cai, &command), "Allocate transfer commands");

    VkPhysicalDeviceIDProperties ids{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES};
    VkPhysicalDeviceProperties2 props{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &ids};
    vkGetPhysicalDeviceProperties2(context->phys_dev, &props);
    dawn::native::vulkan::RequestAdapterOptionsVulkanUUIDs uuid;
    std::copy(ids.deviceUUID, ids.deviceUUID + VK_UUID_SIZE, uuid.deviceUUID.begin());
    std::copy(ids.driverUUID, ids.driverUUID + VK_UUID_SIZE, uuid.driverUUID.begin());
    wgpu::RequestAdapterOptions options;
    options.backendType = wgpu::BackendType::Vulkan;
    options.nextInChain = &uuid;
    auto adapters = instance.EnumerateAdapters(&options);
    if (adapters.empty()) throw std::runtime_error("No Dawn Vulkan adapter matches FFmpeg device UUID");
    wgpu::Adapter adapter(adapters[0].Get());
    wgpu::AdapterInfo info;
    adapter.GetInfo(&info);
    if (info.adapterType != wgpu::AdapterType::DiscreteGPU) throw std::runtime_error("Dawn software adapter forbidden");
    std::cout << "Dawn adapter: " << std::string(info.device.data, info.device.length) << "\n";
    std::array features{wgpu::FeatureName::SharedTextureMemoryOpaqueFD, wgpu::FeatureName::SharedFenceVkSemaphoreOpaqueFD};
    for (auto feature : features) if (!adapter.HasFeature(feature)) throw std::runtime_error("Dawn native FD sharing unsupported");
    wgpu::DeviceDescriptor descriptor;
    wgpu::Limits supported;
    adapter.GetLimits(&supported);
    wgpu::Limits required;
    required.maxStorageBufferBindingSize = supported.maxStorageBufferBindingSize;
    required.maxBufferSize = supported.maxBufferSize;
    descriptor.requiredLimits = &required;
    descriptor.requiredFeatureCount = features.size();
    descriptor.requiredFeatures = features.data();
    descriptor.SetUncapturedErrorCallback([](const wgpu::Device&, wgpu::ErrorType, wgpu::StringView message) {
        std::cerr << "Dawn validation: " << std::string(message.data, message.length) << "\n";
        std::abort();
    });
    dawnDevice = wgpu::Device::Acquire(adapters[0].CreateDevice(&descriptor));
    if (!dawnDevice) throw std::runtime_error("Dawn device creation failed");
    images[0] = makeImage(width, height, VK_FORMAT_R8_UNORM, wgpu::TextureFormat::R8Unorm, false);
    images[1] = makeImage(width / 2, height / 2, VK_FORMAT_R8G8_UNORM, wgpu::TextureFormat::RG8Unorm, false);
    images[2] = makeImage(width / 4, height * 3 / 2, VK_FORMAT_R32_UINT, wgpu::TextureFormat::R32Uint, true);

    VkBufferCreateInfo bci{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
    bci.size = uint64_t(width) * height * 3 / 2;
    bci.usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    vkcheck(vkCreateBuffer(device, &bci, nullptr, &packed), "GPU packed buffer");
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(device, packed, &req);
    VkMemoryAllocateInfo mai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    mai.allocationSize = req.size;
    mai.memoryTypeIndex = memoryType(req.memoryTypeBits);
    vkcheck(vkAllocateMemory(device, &mai, nullptr, &packedMemory), "GPU packed allocation");
    vkcheck(vkBindBufferMemory(device, packed, packedMemory, 0), "Bind GPU packed memory");
}

SharedImage Bridge::makeImage(uint32_t w, uint32_t h, VkFormat format, wgpu::TextureFormat dawnFormat, bool storage) {
    SharedImage image;
    VkExternalMemoryImageCreateInfo external{VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO};
    external.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
    image.create = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
    image.create.pNext = &external;
    image.create.imageType = VK_IMAGE_TYPE_2D;
    image.create.format = format;
    image.create.extent = {w, h, 1};
    image.create.mipLevels = 1;
    image.create.arrayLayers = 1;
    image.create.samples = VK_SAMPLE_COUNT_1_BIT;
    image.create.tiling = VK_IMAGE_TILING_OPTIMAL;
    image.create.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
                         VK_IMAGE_USAGE_SAMPLED_BIT | (storage ? VK_IMAGE_USAGE_STORAGE_BIT : 0);
    vkcheck(vkCreateImage(device, &image.create, nullptr, &image.image), "Create shareable image");
    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(device, image.image, &req);
    VkMemoryDedicatedAllocateInfo dedicated{VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO};
    dedicated.image = image.image;
    VkExportMemoryAllocateInfo exportInfo{VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO, &dedicated};
    exportInfo.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
    VkMemoryAllocateInfo allocation{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &exportInfo};
    allocation.allocationSize = req.size;
    allocation.memoryTypeIndex = memoryType(req.memoryTypeBits);
    vkcheck(vkAllocateMemory(device, &allocation, nullptr, &image.memory), "Allocate shareable memory");
    vkcheck(vkBindImageMemory(device, image.image, image.memory, 0), "Bind shareable memory");
    VkMemoryGetFdInfoKHR getFd{VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR};
    getFd.memory = image.memory;
    getFd.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
    int fd = -1;
    auto getMemoryFd = reinterpret_cast<PFN_vkGetMemoryFdKHR>(vkGetDeviceProcAddr(device, "vkGetMemoryFdKHR"));
    vkcheck(getMemoryFd(device, &getFd, &fd), "Export GPU memory FD");
    // The creation descriptor is consumed synchronously, not retained by Dawn.
    wgpu::SharedTextureMemoryOpaqueFDDescriptor opaque;
    opaque.vkImageCreateInfo = &image.create;
    opaque.memoryFD = fd;
    opaque.memoryTypeIndex = allocation.memoryTypeIndex;
    opaque.allocationSize = allocation.allocationSize;
    opaque.dedicatedAllocation = true;
    wgpu::SharedTextureMemoryDescriptor desc;
    desc.nextInChain = &opaque;
    image.shared = dawnDevice.ImportSharedTextureMemory(&desc);
    close(fd);
    image.create.pNext = nullptr;
    wgpu::TextureDescriptor texture;
    texture.size = {w, h, 1};
    texture.format = dawnFormat;
    texture.usage = storage ? wgpu::TextureUsage::StorageBinding : wgpu::TextureUsage::TextureBinding;
    image.texture = image.shared.CreateTexture(&texture);
    image.view = image.texture.CreateView();
    return image;
}

void Bridge::start() {
    vkcheck(vkResetCommandBuffer(command, 0), "Reset transfer commands");
    VkCommandBufferBeginInfo begin{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkcheck(vkBeginCommandBuffer(command, &begin), "Begin transfer");
}

void Bridge::barrier(VkImage image, VkImageLayout oldLayout, VkImageLayout newLayout,
                     VkAccessFlags src, VkAccessFlags dst, uint32_t sourceFamily, uint32_t targetFamily) {
    VkImageMemoryBarrier b{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER};
    b.srcAccessMask = src;
    b.dstAccessMask = dst;
    b.oldLayout = oldLayout;
    b.newLayout = newLayout;
    b.srcQueueFamilyIndex = sourceFamily;
    b.dstQueueFamilyIndex = targetFamily;
    b.image = image;
    b.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdPipelineBarrier(command, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                         0, 0, nullptr, 0, nullptr, 1, &b);
}

void Bridge::submit(AVVkFrame* frame, const std::vector<VkSemaphore>& waits) {
    vkcheck(vkEndCommandBuffer(command), "End transfer");
    std::vector<VkSemaphore> semaphores = waits;
    std::vector<uint64_t> values(waits.size(), 0), signals;
    int count = 0;
    if (frame) for (; count < AV_NUM_DATA_POINTERS && frame->img[count]; ++count) {
        semaphores.push_back(frame->sem[count]);
        values.push_back(frame->sem_value[count]);
        signals.push_back(frame->sem_value[count] + 1);
    }
    std::vector<VkPipelineStageFlags> stages(semaphores.size(), VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
    VkTimelineSemaphoreSubmitInfo timeline{VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO};
    timeline.waitSemaphoreValueCount = values.size();
    timeline.pWaitSemaphoreValues = values.data();
    timeline.signalSemaphoreValueCount = signals.size();
    timeline.pSignalSemaphoreValues = signals.data();
    VkSubmitInfo submit{VK_STRUCTURE_TYPE_SUBMIT_INFO, &timeline};
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &command;
    submit.waitSemaphoreCount = semaphores.size();
    submit.pWaitSemaphores = semaphores.data();
    submit.pWaitDstStageMask = stages.data();
    submit.signalSemaphoreCount = count;
    submit.pSignalSemaphores = frame ? frame->sem : nullptr;
    context->lock_queue(avdevice, family, 0);
    auto result = vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE);
    if (result == VK_SUCCESS) result = vkQueueWaitIdle(queue);
    context->unlock_queue(avdevice, family, 0);
    vkcheck(result, "Submit GPU-local transfer");
    if (frame) for (int i = 0; i < count; ++i) frame->sem_value[i]++;
}

void Bridge::beginDawn() {
    for (auto& image : images) {
        wgpu::SharedTextureMemoryVkImageLayoutBeginState layout;
        layout.oldLayout = VK_IMAGE_LAYOUT_GENERAL;
        layout.newLayout = VK_IMAGE_LAYOUT_GENERAL;
        wgpu::SharedTextureMemoryBeginAccessDescriptor begin;
        begin.nextInChain = &layout;
        begin.initialized = image.initialized;
        if (image.shared.BeginAccess(image.texture, &begin) != wgpu::Status::Success)
            throw std::runtime_error("Dawn BeginAccess failed");
    }
}

std::vector<VkSemaphore> Bridge::endDawn() {
    std::vector<VkSemaphore> waits;
    for (auto& image : images) {
        wgpu::SharedTextureMemoryVkImageLayoutEndState layout;
        wgpu::SharedTextureMemoryEndAccessState end;
        end.nextInChain = &layout;
        if (image.shared.EndAccess(image.texture, &end) != wgpu::Status::Success)
            throw std::runtime_error("Dawn EndAccess failed");
        if (end.signaledValueCount != end.fenceCount) throw std::runtime_error("Invalid Dawn completion fence values");
        for (size_t i = 0; i < end.fenceCount; ++i) {
            if (end.signaledValues[i] != 1) throw std::runtime_error("Expected binary external fence");
            wgpu::SharedFenceVkSemaphoreOpaqueFDExportInfo opaque;
            wgpu::SharedFenceExportInfo info;
            info.nextInChain = &opaque;
            end.fences[i].ExportInfo(&info);
            VkSemaphore semaphore;
            VkSemaphoreCreateInfo create{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
            vkcheck(vkCreateSemaphore(device, &create, nullptr, &semaphore), "Create imported fence");
            VkImportSemaphoreFdInfoKHR import{VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_FD_INFO_KHR};
            import.semaphore = semaphore;
            import.handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
            import.fd = dup(opaque.handle);
            if (import.fd < 0) throw std::runtime_error("Cannot duplicate Dawn completion FD");
            auto importFd = reinterpret_cast<PFN_vkImportSemaphoreFdKHR>(vkGetDeviceProcAddr(device, "vkImportSemaphoreFdKHR"));
            vkcheck(importFd(device, &import), "Import Dawn completion fence");
            waits.push_back(semaphore);
        }
        barrier(image.image, static_cast<VkImageLayout>(layout.oldLayout), static_cast<VkImageLayout>(layout.newLayout),
                0, VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT, VK_QUEUE_FAMILY_EXTERNAL, family);
        image.layout = static_cast<VkImageLayout>(layout.newLayout);
        image.initialized = true;
    }
    return waits;
}

void Bridge::receive(AVFrame* input) {
    auto* fc = reinterpret_cast<AVHWFramesContext*>(input->hw_frames_ctx->data);
    auto* frame = reinterpret_cast<AVVkFrame*>(input->data[0]);
    if (fc->sw_format != AV_PIX_FMT_NV12 || input->width != width || input->height != height)
        throw std::runtime_error("Only fixed-raster GPU NV12 input is implemented");
    FrameLock lock(fc, frame);
    for (int i = 0; i < AV_NUM_DATA_POINTERS && frame->img[i]; ++i)
        if (frame->queue_family[i] != VK_QUEUE_FAMILY_IGNORED)
            throw std::runtime_error("FFmpeg concurrent image sharing is required");
    start();
    for (int i = 0; i < AV_NUM_DATA_POINTERS && frame->img[i]; ++i) {
        barrier(frame->img[i], frame->layout[i], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                frame->access[i], VK_ACCESS_TRANSFER_READ_BIT);
        frame->layout[i] = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
        frame->access[i] = VK_ACCESS_TRANSFER_READ_BIT;
    }
    for (unsigned plane = 0; plane < 2; ++plane) {
        auto& image = images[plane];
        barrier(image.image, image.layout, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_ACCESS_MEMORY_READ_BIT, VK_ACCESS_TRANSFER_WRITE_BIT);
        VkImageCopy copy{};
        bool multi = frame->img[1] == VK_NULL_HANDLE;
        copy.srcSubresource = {multi ? (plane ? VK_IMAGE_ASPECT_PLANE_1_BIT : VK_IMAGE_ASPECT_PLANE_0_BIT) : VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
        copy.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
        copy.extent = {width >> plane, height >> plane, 1};
        vkCmdCopyImage(command, frame->img[multi ? 0 : plane], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                       image.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &copy);
        image.layout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
        image.initialized = true;
    }
    for (auto& image : images) {
        barrier(image.image, image.layout, VK_IMAGE_LAYOUT_GENERAL, VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_MEMORY_READ_BIT, 0);
        barrier(image.image, VK_IMAGE_LAYOUT_GENERAL, VK_IMAGE_LAYOUT_GENERAL, VK_ACCESS_MEMORY_WRITE_BIT, 0, family, VK_QUEUE_FAMILY_EXTERNAL);
        image.layout = VK_IMAGE_LAYOUT_GENERAL;
    }
    submit(frame);
    beginDawn();
}

void Bridge::deliver(AVFrame* output) {
    auto* fc = reinterpret_cast<AVHWFramesContext*>(output->hw_frames_ctx->data);
    auto* frame = reinterpret_cast<AVVkFrame*>(output->data[0]);
    FrameLock lock(fc, frame);
    start();
    auto waits = endDawn();
    auto& image = images[2];
    barrier(image.image, image.layout, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, 0, VK_ACCESS_TRANSFER_READ_BIT);
    image.layout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    VkBufferImageCopy copy{};
    copy.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    copy.imageExtent = {width / 4, height * 3 / 2, 1};
    vkCmdCopyImageToBuffer(command, image.image, image.layout, packed, 1, &copy);
    VkMemoryBarrier memory{VK_STRUCTURE_TYPE_MEMORY_BARRIER};
    memory.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    memory.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
    vkCmdPipelineBarrier(command, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &memory, 0, nullptr, 0, nullptr);
    for (int i = 0; i < AV_NUM_DATA_POINTERS && frame->img[i]; ++i) {
        if (frame->queue_family[i] != VK_QUEUE_FAMILY_IGNORED) throw std::runtime_error("Output pool must share concurrent images");
        barrier(frame->img[i], frame->layout[i], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, frame->access[i], VK_ACCESS_TRANSFER_WRITE_BIT);
        frame->layout[i] = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
        frame->access[i] = VK_ACCESS_TRANSFER_WRITE_BIT;
    }
    for (unsigned plane = 0; plane < 2; ++plane) {
        bool multi = frame->img[1] == VK_NULL_HANDLE;
        copy = {};
        copy.bufferOffset = plane ? uint64_t(width) * height : 0;
        copy.imageSubresource = {multi ? (plane ? VK_IMAGE_ASPECT_PLANE_1_BIT : VK_IMAGE_ASPECT_PLANE_0_BIT) : VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
        copy.imageExtent = {width >> plane, height >> plane, 1};
        vkCmdCopyBufferToImage(command, packed, frame->img[multi ? 0 : plane], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &copy);
    }
    submit(frame, waits);
    for (auto semaphore : waits) vkDestroySemaphore(device, semaphore, nullptr);
}

Bridge::~Bridge() {
    vkDeviceWaitIdle(device);
    for (auto& image : images) {
        image.view = nullptr;
        image.texture = nullptr;
        image.shared = nullptr;
    }
    dawnDevice = nullptr;
    for (auto& image : images) {
        vkDestroyImage(device, image.image, nullptr);
        vkFreeMemory(device, image.memory, nullptr);
    }
    vkDestroyBuffer(device, packed, nullptr);
    vkFreeMemory(device, packedMemory, nullptr);
    vkDestroyCommandPool(device, pool, nullptr);
}
