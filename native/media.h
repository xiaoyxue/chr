#ifndef CHR_MEDIA_H
#define CHR_MEDIA_H

#include <stdint.h>
#include <vulkan/vulkan.h>

typedef struct ChrMedia ChrMedia;

typedef struct ChrFrameInfo {
    int64_t pts;
    int32_t time_base_num;
    int32_t time_base_den;
    uint32_t width;
    uint32_t height;
    uint32_t color_mode;
    uint32_t reserved;
} ChrFrameInfo;

ChrMedia *chr_open(const char *input, uint64_t instance, uint64_t physical,
                   uint64_t device, uintptr_t get_proc_addr,
                   const char *const *instance_extensions, int instance_count,
                   const char *const *device_extensions, int device_count,
                   uint32_t decode_family, uint32_t encode_family,
                   const VkPhysicalDeviceFeatures *features);
int chr_next(ChrMedia *media, ChrFrameInfo *info);
int chr_rewind(ChrMedia *media);
int chr_open_output(ChrMedia *media, const char *url, int fps, int bitrate);
int chr_copy_input(ChrMedia *media, uint64_t y_image, uint64_t uv_image);
int chr_encode(ChrMedia *media, uint64_t packed_nv12_buffer, int64_t sequence);
int chr_finish(ChrMedia *media);
void chr_lock_queue(ChrMedia *media);
void chr_unlock_queue(ChrMedia *media);
int chr_stopped(void);
int chr_verify_source(const char *path, const char *sha256);
void chr_close(ChrMedia *media);

#endif
