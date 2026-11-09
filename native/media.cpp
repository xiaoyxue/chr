#include "media.hpp"
#include <iostream>
#include <chrono>
#include <thread>

static AVPixelFormat hardware_format(AVCodecContext*, const AVPixelFormat* formats) {
    for (auto f = formats; *f != AV_PIX_FMT_NONE; ++f)
        if (*f == AV_PIX_FMT_VULKAN) return *f;
    return AV_PIX_FMT_NONE;
}

Source::Source(const std::string& path) {
    avcheck(av_hwdevice_ctx_create(&device, AV_HWDEVICE_TYPE_VULKAN, "NVIDIA GeForce RTX 3090", nullptr, 0), "Vulkan device");
    auto* context = reinterpret_cast<AVHWDeviceContext*>(device->data);
    auto* vk = reinterpret_cast<AVVulkanDeviceContext*>(context->hwctx);
    VkPhysicalDeviceProperties properties;
    vkGetPhysicalDeviceProperties(vk->phys_dev, &properties);
    if (properties.deviceType != VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU ||
        std::string(properties.deviceName).find("RTX 3090") == std::string::npos)
        throw std::runtime_error("RTX 3090 hardware is required; software fallback forbidden");
    std::cout << "FFmpeg adapter: " << properties.deviceName << "\n";
    avcheck(avformat_open_input(&format, path.c_str(), nullptr, nullptr), "Open MP4");
    if (std::string(format->iformat->name) != "mov,mp4,m4a,3gp,3g2,mj2")
        throw std::runtime_error("Only header-complete MP4 input is supported");
    stream = av_find_best_stream(format, AVMEDIA_TYPE_VIDEO, -1, -1, nullptr, 0);
    avcheck(stream, "Video stream");
    requireCompleteVideoHeader(*format->streams[stream]->codecpar, format->streams[stream]->time_base);
    std::cout << "Input metadata: MP4 header only; frame probing disabled\n";
    auto* codec = avcodec_find_decoder(format->streams[stream]->codecpar->codec_id);
    decoder = avcodec_alloc_context3(codec);
    avcheck(avcodec_parameters_to_context(decoder, format->streams[stream]->codecpar), "Decoder parameters");
    decoder->hw_device_ctx = av_buffer_ref(device);
    decoder->get_format = hardware_format;
    decoder->thread_count = 1;
    avcheck(avcodec_open2(decoder, codec, nullptr), "Hardware decoder");
}

Source::~Source() {
    avcodec_free_context(&decoder);
    avformat_close_input(&format);
    av_buffer_unref(&device);
}

bool Source::next(AVFrame* frame) {
    AVPacket* packet = av_packet_alloc();
    while (true) {
        int result = avcodec_receive_frame(decoder, frame);
        if (result == 0) {
            av_packet_free(&packet);
            if (frame->format != AV_PIX_FMT_VULKAN || !frame->hw_frames_ctx)
                throw std::runtime_error("Decoder returned a non-Vulkan frame");
            return true;
        }
        if (result == AVERROR_EOF) { av_packet_free(&packet); return false; }
        avcheck(result == AVERROR(EAGAIN) ? 0 : result, "Receive hardware frame");
        if (draining) throw std::runtime_error("Decoder stalled while draining");
        result = av_read_frame(format, packet);
        if (result == AVERROR_EOF) {
            draining = true;
            avcheck(avcodec_send_packet(decoder, nullptr), "Flush decoder");
        } else {
            avcheck(result, "Read compressed packet");
            if (packet->stream_index == stream)
                avcheck(avcodec_send_packet(decoder, packet), "Decode compressed packet");
            av_packet_unref(packet);
        }
    }
}

void Source::rewind() {
    avcheck(av_seek_frame(format, stream, 0, AVSEEK_FLAG_BACKWARD), "Loop seek");
    avcodec_flush_buffers(decoder);
    draining = false;
}

Output::Output(const std::string& path, AVFrame* frame, AVBufferRef* device) {
    srt = path.starts_with("srt://");
    avcheck(avformat_alloc_output_context2(&format, nullptr, "mpegts", path.c_str()), "MPEG-TS muxer");
    const AVCodec* codec = avcodec_find_encoder_by_name("h264_vulkan");
    if (!codec) throw std::runtime_error("h264_vulkan encoder is unavailable");
    encoder = avcodec_alloc_context3(codec);
    encoder->width = frame->width;
    encoder->height = frame->height;
    encoder->pix_fmt = AV_PIX_FMT_VULKAN;
    encoder->time_base = AVRational{1, 30};
    encoder->framerate = AVRational{30, 1};
    encoder->bit_rate = 12000000;
    encoder->gop_size = 30;
    encoder->max_b_frames = 0;
    encoder->color_range = frame->color_range;
    encoder->colorspace = frame->colorspace;
    encoder->color_primaries = frame->color_primaries;
    encoder->color_trc = frame->color_trc;
    encoder->hw_device_ctx = av_buffer_ref(device);
    encoder->hw_frames_ctx = av_buffer_ref(frame->hw_frames_ctx);
    if (format->oformat->flags & AVFMT_GLOBALHEADER) encoder->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    avcheck(avcodec_open2(encoder, codec, nullptr), "Vulkan hardware encoder");
    stream = avformat_new_stream(format, nullptr);
    stream->time_base = encoder->time_base;
    avcheck(avcodec_parameters_from_context(stream->codecpar, encoder), "Output parameters");
    avcheck(avio_open(&format->pb, path.c_str(), AVIO_FLAG_WRITE), "Open MPEG-TS output");
    avcheck(avformat_write_header(format, nullptr), "MPEG-TS header");
}

void Output::drain() {
    AVPacket* packet = av_packet_alloc();
    while (true) {
        int result = avcodec_receive_packet(encoder, packet);
        if (result == AVERROR(EAGAIN) || result == AVERROR_EOF) break;
        avcheck(result, "Receive encoded packet");
        av_packet_rescale_ts(packet, encoder->time_base, stream->time_base);
        packet->stream_index = stream->index;
        avcheck(av_interleaved_write_frame(format, packet), "Mux encoded packet");
        avio_flush(format->pb);
        avcheck(format->pb->error, "Write encoded transport");
        av_packet_unref(packet);
    }
    av_packet_free(&packet);
}
void Output::send(AVFrame* frame) { avcheck(avcodec_send_frame(encoder, frame), "Encode GPU frame"); drain(); }
void Output::finish() {
    send(nullptr);
    avcheck(av_write_trailer(format), "MPEG-TS trailer");
    avio_flush(format->pb);
    if (format->pb->error) avcheck(format->pb->error, "Flush transport");
    // Live SRT delivery is delayed at the receiver. Closing immediately drops its tail.
    if (srt) std::this_thread::sleep_for(std::chrono::seconds(1));
}
Output::~Output() {
    avcodec_free_context(&encoder);
    if (format) { avio_closep(&format->pb); avformat_free_context(format); }
}
