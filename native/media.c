#include "media.h"

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/error.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vulkan.h>
#include <libavutil/mem.h>
#include <libavutil/sha.h>
#include <libavutil/time.h>

struct ChrMedia {
    AVBufferRef *device;
    AVBufferRef *encode_pool;
    AVFormatContext *input;
    AVFormatContext *output;
    AVCodecContext *decoder;
    AVCodecContext *encoder;
    AVFrame *frame;
    AVFrame *encoded_frame;
    AVPacket *packet;
    AVPacket *encoded_packet;
    int video_stream;
    int draining;
    int header_written;
    VkDevice vk_device;
    VkQueue queue;
    VkCommandPool command_pool;
    VkCommandBuffer command;
    VkFence fence;
    VkPhysicalDeviceTimelineSemaphoreFeatures timeline;
    VkPhysicalDeviceSynchronization2Features sync;
    VkPhysicalDeviceVideoMaintenance1FeaturesKHR video_maintenance;
    const char **instance_extensions;
    const char **device_extensions;
};

static volatile sig_atomic_t stopped;

static void stop_handler(int signal_number) {
    (void)signal_number;
    stopped = 1;
}

int chr_stopped(void) { return stopped; }

int chr_verify_source(const char *path, const char *expected) {
    FILE *file = fopen(path, "rb");
    if (!file) return AVERROR(errno);
    struct AVSHA *sha = av_sha_alloc();
    if (!sha) {
        fclose(file);
        return AVERROR(ENOMEM);
    }
    av_sha_init(sha, 256);
    unsigned char buffer[65536], digest[32];
    size_t count;
    while ((count = fread(buffer, 1, sizeof(buffer), file)))
        av_sha_update(sha, buffer, count);
    int result = ferror(file) ? AVERROR(EIO) : 0;
    fclose(file);
    av_sha_final(sha, digest);
    av_free(sha);
    char actual[65];
    for (int i = 0; i < 32; i++) snprintf(actual + 2 * i, 3, "%02x", digest[i]);
    if (strcmp(actual, expected)) result = AVERROR_INVALIDDATA;
    return result;
}

static int interrupt(void *opaque) {
    (void)opaque;
    return stopped;
}

static int failure(const char *operation, int error) {
    char message[AV_ERROR_MAX_STRING_SIZE];
    av_strerror(error, message, sizeof(message));
    fprintf(stderr, "%s: %s (%d)\n", operation, message, error);
    return error < 0 ? error : AVERROR_EXTERNAL;
}

#define AV_TRY(expression) do { \
    int result = (expression); \
    if (result < 0) return failure(#expression, result); \
} while (0)

static enum AVPixelFormat hardware_format(AVCodecContext *context,
                                          const enum AVPixelFormat *formats) {
    (void)context;
    for (; *formats != AV_PIX_FMT_NONE; formats++) {
        if (*formats == AV_PIX_FMT_VULKAN) return *formats;
    }
    return AV_PIX_FMT_NONE;
}

void chr_lock_queue(ChrMedia *media) {
    AVHWDeviceContext *device = (AVHWDeviceContext *)media->device->data;
    AVVulkanDeviceContext *vk = device->hwctx;
    vk->lock_queue(device, 0, 0);
}

void chr_unlock_queue(ChrMedia *media) {
    AVHWDeviceContext *device = (AVHWDeviceContext *)media->device->data;
    AVVulkanDeviceContext *vk = device->hwctx;
    vk->unlock_queue(device, 0, 0);
}

static int initialize(ChrMedia *media, const char *input, uint64_t instance,
                      uint64_t physical, uint64_t device, uintptr_t get_proc_addr,
                      const char *const *instance_extensions, int instance_count,
                      const char *const *device_extensions, int device_count,
                      uint32_t decode_family, uint32_t encode_family,
                      const VkPhysicalDeviceFeatures *features) {
    media->device = av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_VULKAN);
    if (!media->device) return AVERROR(ENOMEM);
    AVHWDeviceContext *hardware = (AVHWDeviceContext *)media->device->data;
    AVVulkanDeviceContext *vk = hardware->hwctx;
    vk->inst = (VkInstance)(uintptr_t)instance;
    vk->phys_dev = (VkPhysicalDevice)(uintptr_t)physical;
    vk->act_dev = media->vk_device = (VkDevice)(uintptr_t)device;
    vk->get_proc_addr = (PFN_vkGetInstanceProcAddr)get_proc_addr;
    media->timeline = (VkPhysicalDeviceTimelineSemaphoreFeatures){
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES,
        .timelineSemaphore = VK_TRUE,
    };
    media->sync = (VkPhysicalDeviceSynchronization2Features){
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SYNCHRONIZATION_2_FEATURES,
        .pNext = &media->timeline,
        .synchronization2 = VK_TRUE,
    };
    media->video_maintenance = (VkPhysicalDeviceVideoMaintenance1FeaturesKHR){
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VIDEO_MAINTENANCE_1_FEATURES_KHR,
        .pNext = &media->sync,
        .videoMaintenance1 = VK_TRUE,
    };
    vk->device_features = (VkPhysicalDeviceFeatures2){
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
        .pNext = &media->video_maintenance,
        .features = *features,
    };
    media->instance_extensions = av_memdup(
        instance_extensions, (size_t)instance_count * sizeof(char *));
    media->device_extensions = av_memdup(
        device_extensions, (size_t)device_count * sizeof(char *));
    if ((instance_count && !media->instance_extensions) ||
        (device_count && !media->device_extensions)) return AVERROR(ENOMEM);
    vk->enabled_inst_extensions = media->instance_extensions;
    vk->nb_enabled_inst_extensions = instance_count;
    vk->enabled_dev_extensions = media->device_extensions;
    vk->nb_enabled_dev_extensions = device_count;
    vk->qf[0] = (AVVulkanDeviceQueueFamily){
        .idx = 0, .num = 1,
        .flags = VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT,
    };
    vk->qf[1] = (AVVulkanDeviceQueueFamily){
        .idx = (int)decode_family, .num = 1,
        .flags = VK_QUEUE_VIDEO_DECODE_BIT_KHR | VK_QUEUE_TRANSFER_BIT,
        .video_caps = VK_VIDEO_CODEC_OPERATION_DECODE_H264_BIT_KHR,
    };
    vk->qf[2] = (AVVulkanDeviceQueueFamily){
        .idx = (int)encode_family, .num = 1,
        .flags = VK_QUEUE_VIDEO_ENCODE_BIT_KHR | VK_QUEUE_TRANSFER_BIT,
        .video_caps = VK_VIDEO_CODEC_OPERATION_ENCODE_H264_BIT_KHR,
    };
    vk->nb_qf = 3;
    AV_TRY(av_hwdevice_ctx_init(media->device));
    vkGetDeviceQueue(media->vk_device, 0, 0, &media->queue);
    VkCommandPoolCreateInfo pool = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
        .queueFamilyIndex = 0,
    };
    if (vkCreateCommandPool(media->vk_device, &pool, NULL, &media->command_pool))
        return AVERROR_EXTERNAL;
    VkCommandBufferAllocateInfo allocation = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = media->command_pool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    if (vkAllocateCommandBuffers(media->vk_device, &allocation, &media->command))
        return AVERROR_EXTERNAL;
    VkFenceCreateInfo fence = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    if (vkCreateFence(media->vk_device, &fence, NULL, &media->fence))
        return AVERROR_EXTERNAL;
    media->input = avformat_alloc_context();
    if (!media->input) return AVERROR(ENOMEM);
    media->input->interrupt_callback = (AVIOInterruptCB){interrupt, NULL};
    AV_TRY(avformat_open_input(&media->input, input, NULL, NULL));
    const AVCodec *codec = NULL;
    media->video_stream = av_find_best_stream(
        media->input, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
    AV_TRY(media->video_stream);
    AVCodecParameters *parameters = media->input->streams[media->video_stream]->codecpar;
    if (parameters->width <= 0 || parameters->height <= 0 || parameters->extradata_size <= 0) {
        return failure("MP4 header is incomplete; software-frame probing is forbidden",
                       AVERROR_INVALIDDATA);
    }
    media->decoder = avcodec_alloc_context3(codec);
    if (!media->decoder) return AVERROR(ENOMEM);
    AV_TRY(avcodec_parameters_to_context(
        media->decoder, media->input->streams[media->video_stream]->codecpar));
    media->decoder->get_format = hardware_format;
    media->decoder->hw_device_ctx = av_buffer_ref(media->device);
    media->decoder->thread_count = 1;
    AV_TRY(avcodec_open2(media->decoder, codec, NULL));
    media->frame = av_frame_alloc();
    media->encoded_frame = av_frame_alloc();
    media->packet = av_packet_alloc();
    media->encoded_packet = av_packet_alloc();
    return media->frame && media->encoded_frame && media->packet && media->encoded_packet
        ? 0 : AVERROR(ENOMEM);
}

ChrMedia *chr_open(const char *input, uint64_t instance, uint64_t physical,
                   uint64_t device, uintptr_t get_proc_addr,
                   const char *const *instance_extensions, int instance_count,
                   const char *const *device_extensions, int device_count,
                   uint32_t decode_family, uint32_t encode_family,
                   const VkPhysicalDeviceFeatures *features) {
    umask(0077);
    signal(SIGINT, stop_handler);
    signal(SIGTERM, stop_handler);
    av_log_set_level(AV_LOG_WARNING);
    ChrMedia *media = av_mallocz(sizeof(*media));
    if (!media) return NULL;
    int result = initialize(media, input, instance, physical, device, get_proc_addr,
                            instance_extensions, instance_count, device_extensions,
                            device_count, decode_family, encode_family, features);
    if (result < 0) {
        failure("Initialize FFmpeg Vulkan media", result);
        chr_close(media);
        return NULL;
    }
    return media;
}

int chr_next(ChrMedia *media, ChrFrameInfo *info) {
    av_frame_unref(media->frame);
    while (!stopped) {
        int result = avcodec_receive_frame(media->decoder, media->frame);
        if (result == 0) {
            if (media->frame->format != AV_PIX_FMT_VULKAN)
                return failure("Refusing non-GPU decoded frame", AVERROR(EINVAL));
            AVHWFramesContext *pool = (AVHWFramesContext *)media->frame->hw_frames_ctx->data;
            if (pool->sw_format != AV_PIX_FMT_NV12)
                return failure("Expected GPU NV12", AVERROR(EINVAL));
            AVRational time_base = media->input->streams[media->video_stream]->time_base;
            *info = (ChrFrameInfo){
                .pts = media->frame->best_effort_timestamp,
                .time_base_num = time_base.num, .time_base_den = time_base.den,
                .width = media->frame->width, .height = media->frame->height,
                .color_mode = (media->frame->colorspace == AVCOL_SPC_BT709 ? 1 : 0)
                    | (media->frame->color_range == AVCOL_RANGE_JPEG ? 2 : 0),
            };
            return 1;
        }
        if (result == AVERROR_EOF) return 0;
        if (result != AVERROR(EAGAIN)) return failure("Receive hardware frame", result);
        if (media->draining) return failure("Unexpected decoder drain", AVERROR(EINVAL));
        do {
            av_packet_unref(media->packet);
            result = av_read_frame(media->input, media->packet);
        } while (result >= 0 && media->packet->stream_index != media->video_stream);
        if (result == AVERROR_EOF) {
            media->draining = 1;
            AV_TRY(avcodec_send_packet(media->decoder, NULL));
        } else {
            AV_TRY(result);
            AV_TRY(avcodec_send_packet(media->decoder, media->packet));
        }
    }
    return AVERROR_EXIT;
}

int chr_rewind(ChrMedia *media) {
    AV_TRY(av_seek_frame(media->input, media->video_stream, 0, AVSEEK_FLAG_BACKWARD));
    avcodec_flush_buffers(media->decoder);
    media->draining = 0;
    return 0;
}

int chr_open_output(ChrMedia *media, const char *url, int fps, int bitrate) {
    media->encode_pool = av_hwframe_ctx_alloc(media->device);
    if (!media->encode_pool) return AVERROR(ENOMEM);
    AVHWFramesContext *pool = (AVHWFramesContext *)media->encode_pool->data;
    pool->format = AV_PIX_FMT_VULKAN;
    pool->sw_format = AV_PIX_FMT_NV12;
    pool->width = media->decoder->width;
    pool->height = media->decoder->height;
    pool->initial_pool_size = 4;
    AVVulkanFramesContext *vk_pool = pool->hwctx;
    vk_pool->format[0] = VK_FORMAT_G8_B8R8_2PLANE_420_UNORM;
    vk_pool->usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_VIDEO_ENCODE_SRC_BIT_KHR;
    AV_TRY(av_hwframe_ctx_init(media->encode_pool));
    const AVCodec *codec = avcodec_find_encoder_by_name("h264_vulkan");
    if (!codec) return failure("h264_vulkan encoder is unavailable", AVERROR(ENOSYS));
    media->encoder = avcodec_alloc_context3(codec);
    if (!media->encoder) return AVERROR(ENOMEM);
    media->encoder->width = pool->width;
    media->encoder->height = pool->height;
    media->encoder->pix_fmt = AV_PIX_FMT_VULKAN;
    media->encoder->sw_pix_fmt = AV_PIX_FMT_NV12;
    media->encoder->time_base = (AVRational){1, fps};
    media->encoder->framerate = (AVRational){fps, 1};
    media->encoder->bit_rate = bitrate;
    media->encoder->gop_size = fps;
    media->encoder->max_b_frames = 0;
    media->encoder->hw_frames_ctx = av_buffer_ref(media->encode_pool);
    media->encoder->colorspace = media->decoder->colorspace == AVCOL_SPC_UNSPECIFIED
        ? AVCOL_SPC_SMPTE170M : media->decoder->colorspace;
    media->encoder->color_range = media->decoder->color_range == AVCOL_RANGE_UNSPECIFIED
        ? AVCOL_RANGE_MPEG : media->decoder->color_range;
    AV_TRY(avcodec_open2(media->encoder, codec, NULL));
    AV_TRY(avformat_alloc_output_context2(&media->output, NULL, "mpegts", url));
    media->output->interrupt_callback = (AVIOInterruptCB){interrupt, NULL};
    media->output->flags |= AVFMT_FLAG_FLUSH_PACKETS;
    media->output->max_delay = 0;
    AVStream *stream = avformat_new_stream(media->output, NULL);
    if (!stream) return AVERROR(ENOMEM);
    stream->time_base = (AVRational){1, 90000};
    AV_TRY(avcodec_parameters_from_context(stream->codecpar, media->encoder));
    fprintf(stderr, "Opening output (an SRT listener waits for its player): %s\n", url);
    AV_TRY(avio_open2(&media->output->pb, url, AVIO_FLAG_WRITE,
                      &media->output->interrupt_callback, NULL));
    AV_TRY(avformat_write_header(media->output, NULL));
    media->header_written = 1;
    return 0;
}

static int transfer(ChrMedia *media, AVFrame *frame, VkImage y, VkImage uv,
                     VkBuffer packed, int to_encoder) {
    AVHWFramesContext *pool = (AVHWFramesContext *)frame->hw_frames_ctx->data;
    AVVulkanFramesContext *vk_pool = pool->hwctx;
    AVVkFrame *image = (AVVkFrame *)frame->data[0];
    int count = image->img[1] != VK_NULL_HANDLE ? 2 : 1;
    VkSemaphore semaphores[2];
    uint64_t values[2], signals[2];
    VkPipelineStageFlags stages[2] = {VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT};
    vk_pool->lock_frame(pool, image);
    for (int i = 0; i < count; i++) {
        if (image->queue_family[i] != VK_QUEUE_FAMILY_IGNORED) {
            vk_pool->unlock_frame(pool, image);
            return failure("Expected concurrently shared FFmpeg images", AVERROR(ENOSYS));
        }
        semaphores[i] = image->sem[i];
        values[i] = image->sem_value[i];
        signals[i] = values[i] + 1;
    }
    vkResetCommandBuffer(media->command, 0);
    VkCommandBufferBeginInfo begin = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    VkResult result = vkBeginCommandBuffer(media->command, &begin);
    if (result) {
        vk_pool->unlock_frame(pool, image);
        return AVERROR_EXTERNAL;
    }
    VkImageLayout layout = to_encoder ? VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL
                                      : VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    VkAccessFlags access = to_encoder ? VK_ACCESS_TRANSFER_WRITE_BIT : VK_ACCESS_TRANSFER_READ_BIT;
    for (int i = 0; i < count; i++) {
        VkImageMemoryBarrier barrier = {
            .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
            .srcAccessMask = image->access[i], .dstAccessMask = access,
            .oldLayout = image->layout[i], .newLayout = layout,
            .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .image = image->img[i],
            .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1},
        };
        vkCmdPipelineBarrier(media->command, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                             VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &barrier);
    }
    for (int plane = 0; plane < 2; plane++) {
        VkImage target = image->img[count == 1 ? 0 : plane];
        VkImageAspectFlags aspect = count == 1
            ? (plane ? VK_IMAGE_ASPECT_PLANE_1_BIT : VK_IMAGE_ASPECT_PLANE_0_BIT)
            : VK_IMAGE_ASPECT_COLOR_BIT;
        VkExtent3D extent = {(uint32_t)frame->width / (plane ? 2 : 1),
                             (uint32_t)frame->height / (plane ? 2 : 1), 1};
        if (to_encoder) {
            VkBufferImageCopy copy = {
                .bufferOffset = plane ? (VkDeviceSize)frame->width * frame->height : 0,
                .imageSubresource = {aspect, 0, 0, 1},
                .imageExtent = extent,
            };
            vkCmdCopyBufferToImage(media->command, packed, target, layout, 1, &copy);
        } else {
            VkImageCopy copy = {
                .srcSubresource = {aspect, 0, 0, 1},
                .dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
                .extent = extent,
            };
            vkCmdCopyImage(media->command, target, layout, plane ? uv : y,
                           VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &copy);
        }
    }
    result = vkEndCommandBuffer(media->command);
    VkTimelineSemaphoreSubmitInfo timeline = {
        .sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO,
        .waitSemaphoreValueCount = count, .pWaitSemaphoreValues = values,
        .signalSemaphoreValueCount = count, .pSignalSemaphoreValues = signals,
    };
    VkSubmitInfo submit = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .pNext = &timeline,
        .waitSemaphoreCount = count, .pWaitSemaphores = semaphores,
        .pWaitDstStageMask = stages, .commandBufferCount = 1,
        .pCommandBuffers = &media->command,
        .signalSemaphoreCount = count, .pSignalSemaphores = semaphores,
    };
    vkResetFences(media->vk_device, 1, &media->fence);
    if (!result) {
        chr_lock_queue(media);
        result = vkQueueSubmit(media->queue, 1, &submit, media->fence);
        chr_unlock_queue(media);
    }
    if (!result) {
        for (int i = 0; i < count; i++) {
            image->layout[i] = layout;
            image->access[i] = access;
            image->sem_value[i] = signals[i];
        }
    }
    vk_pool->unlock_frame(pool, image);
    if (!result) result = vkWaitForFences(
        media->vk_device, 1, &media->fence, VK_TRUE, UINT64_C(10000000000));
    if (result) fprintf(stderr, "GPU plane transfer failed: Vulkan status %d\n", result);
    return result == VK_SUCCESS ? 0 : AVERROR_EXTERNAL;
}

int chr_copy_input(ChrMedia *media, uint64_t y_image, uint64_t uv_image) {
    return transfer(media, media->frame, (VkImage)y_image, (VkImage)uv_image,
                     VK_NULL_HANDLE, 0);
}

static int write_packets(ChrMedia *media) {
    for (;;) {
        int result = avcodec_receive_packet(media->encoder, media->encoded_packet);
        if (result == AVERROR(EAGAIN) || result == AVERROR_EOF) return 0;
        AV_TRY(result);
        av_packet_rescale_ts(media->encoded_packet, media->encoder->time_base,
                             media->output->streams[0]->time_base);
        media->encoded_packet->stream_index = 0;
        result = av_interleaved_write_frame(media->output, media->encoded_packet);
        av_packet_unref(media->encoded_packet);
        AV_TRY(result);
    }
}

int chr_encode(ChrMedia *media, uint64_t packed_nv12_buffer, int64_t sequence) {
    av_frame_unref(media->encoded_frame);
    AV_TRY(av_hwframe_get_buffer(media->encode_pool, media->encoded_frame, 0));
    media->encoded_frame->pts = sequence;
    media->encoded_frame->colorspace = media->encoder->colorspace;
    media->encoded_frame->color_range = media->encoder->color_range;
    AV_TRY(transfer(media, media->encoded_frame, VK_NULL_HANDLE, VK_NULL_HANDLE,
                     (VkBuffer)packed_nv12_buffer, 1));
    AV_TRY(avcodec_send_frame(media->encoder, media->encoded_frame));
    return write_packets(media);
}

int chr_finish(ChrMedia *media) {
    AV_TRY(avcodec_send_frame(media->encoder, NULL));
    AV_TRY(write_packets(media));
    AV_TRY(av_write_trailer(media->output));
    avio_flush(media->output->pb);
    if (strncmp(media->output->url, "srt://", 6) == 0 && !stopped) {
        av_usleep(1000000);
    }
    media->header_written = 0;
    return 0;
}

void chr_close(ChrMedia *media) {
    if (!media) return;
    if (media->vk_device) vkDeviceWaitIdle(media->vk_device);
    av_frame_free(&media->frame);
    av_frame_free(&media->encoded_frame);
    av_packet_free(&media->packet);
    av_packet_free(&media->encoded_packet);
    avcodec_free_context(&media->decoder);
    avcodec_free_context(&media->encoder);
    avformat_close_input(&media->input);
    if (media->output) {
        avio_closep(&media->output->pb);
        avformat_free_context(media->output);
    }
    av_buffer_unref(&media->encode_pool);
    if (media->fence) vkDestroyFence(media->vk_device, media->fence, NULL);
    if (media->command_pool) vkDestroyCommandPool(media->vk_device, media->command_pool, NULL);
    av_buffer_unref(&media->device);
    av_free(media->instance_extensions);
    av_free(media->device_extensions);
    av_free(media);
}
