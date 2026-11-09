#pragma once
extern "C" {
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vulkan.h>
}
#include <stdexcept>
#include <string>

inline void avcheck(int result, const char* operation) {
    if (result < 0) {
        char text[AV_ERROR_MAX_STRING_SIZE];
        av_strerror(result, text, sizeof(text));
        throw std::runtime_error(std::string(operation) + ": " + text);
    }
}

inline void requireCompleteVideoHeader(const AVCodecParameters& parameters, AVRational timebase) {
    if (parameters.codec_type != AVMEDIA_TYPE_VIDEO || parameters.codec_id != AV_CODEC_ID_H264 ||
        parameters.width <= 0 || parameters.height <= 0 ||
        !parameters.extradata || parameters.extradata_size <= 0 ||
        timebase.num <= 0 || timebase.den <= 0)
        throw std::runtime_error("Incomplete H.264 MP4 header; software frame probing is forbidden");
}

class Source {
public:
    AVBufferRef* device = nullptr;
    AVFormatContext* format = nullptr;
    AVCodecContext* decoder = nullptr;
    int stream = -1;
    bool draining = false;
    explicit Source(const std::string& path);
    ~Source();
    bool next(AVFrame* frame);
    void rewind();
    AVRational timebase() const { return format->streams[stream]->time_base; }
};

class Output {
    AVFormatContext* format = nullptr;
    AVCodecContext* encoder = nullptr;
    AVStream* stream = nullptr;
    bool srt = false;
    void drain();
public:
    Output(const std::string& path, AVFrame* frame, AVBufferRef* device);
    ~Output();
    void send(AVFrame* frame);
    void finish();
};
