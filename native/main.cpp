#include "renderer.hpp"
#include <chrono>
#include <iostream>
#include <memory>
#include <thread>

struct Options {
    std::string scene, output, ledger;
    uint64_t frames = 0;
    uint32_t mode = 0;
    bool loop = false, pace = false;
};
Options arguments(int argc, char** argv) {
    Options result;
    for (int i = 1; i < argc; ++i) {
        std::string key = argv[i];
        if (key == "--loop") { result.loop = true; continue; }
        if (key == "--pace") { result.pace = true; continue; }
        if (i + 1 == argc) throw std::runtime_error("Missing option value: " + key);
        std::string value = argv[++i];
        if (key == "--scene") result.scene = value;
        else if (key == "--output") result.output = value;
        else if (key == "--ledger") result.ledger = value;
        else if (key == "--frames") {
            size_t end = 0;
            result.frames = std::stoull(value, &end);
            if (value.empty() || value.front() == '-' || end != value.size() || !result.frames)
                throw std::runtime_error("Frame limit must be a positive integer");
        } else if (key == "--mode") {
            if (value == "scene") result.mode = 0;
            else if (value == "bypass") result.mode = 1;
            else if (value == "marker") result.mode = 2;
            else throw std::runtime_error("Mode must be scene, bypass, or marker");
        } else throw std::runtime_error("Unknown option: " + key);
    }
    if (result.scene.empty() || result.output.empty() || result.ledger.empty() || !result.frames)
        throw std::runtime_error("Usage: chr-dawn --scene FILE --output FILE_OR_SRT_URL --ledger FILE --frames N [--loop] [--pace] [--mode scene|bypass|marker]");
    return result;
}

int main(int argc, char** argv) try {
    std::cout << std::unitbuf;
    const auto options = arguments(argc, argv);
    Scene scene(options.scene);
    scene.verifyVideo();
    Source source(scene.manifest.at("video"));
    auto deleter = [](AVFrame* frame) { av_frame_free(&frame); };
    std::unique_ptr<AVFrame, decltype(deleter)> input(av_frame_alloc(), deleter), output(av_frame_alloc(), deleter);
    if (!source.next(input.get())) throw std::runtime_error("Empty MP4");
    if (input->width != scene.width || input->height != scene.height)
        throw std::runtime_error("Source raster does not match prepared scene");
    Bridge bridge(source.device, scene.width, scene.height);
    Renderer renderer(bridge, scene);
    auto allocateOutput = [&] {
        avcheck(av_hwframe_get_buffer(input->hw_frames_ctx, output.get(), 0), "Allocate GPU output");
        avcheck(av_frame_copy_props(output.get(), input.get()), "Copy frame metadata");
        output->width = input->width;
        output->height = input->height;
    };
    allocateOutput();
    Output sink(options.output, output.get(), source.device);
    std::ofstream ledger(options.ledger);
    if (!ledger) throw std::runtime_error("Cannot open identity ledger");
    const auto epoch = std::chrono::system_clock::now().time_since_epoch().count();
    const auto start = std::chrono::steady_clock::now();
    uint64_t processed = 0, loops = 0, withTi = 0, expectedOrdinal = 0;
    for (; processed < options.frames; ++processed) {
        size_t ordinal = scene.associate(input->pts, source.timebase());
        if (ordinal != expectedOrdinal) throw std::runtime_error("Source discontinuity, duplicate, or skipped frame");
        ++expectedOrdinal;
        const auto& entry = scene.manifest.at("frames").at(ordinal);
        if (!entry.at("ti_row_index").is_null()) ++withTi;
        auto data = scene.parameters(ordinal, input.get(), options.mode);
        auto mask = scene.mask(ordinal);
        if (options.pace)
            std::this_thread::sleep_until(start + std::chrono::nanoseconds(processed * 1000000000 / 30));
        bridge.receive(input.get());
        renderer.render(data, mask);
        bridge.deliver(output.get());
        output->pts = processed;
        sink.send(output.get());
        nlohmann::json identity{
            {"stream_id", scene.manifest.at("case")}, {"epoch", epoch}, {"sequence", processed},
            {"timecode", timecode(processed)}, {"fps_num", 30}, {"drop_frame", false},
            {"source_loop", loops}, {"source_pts", input->pts},
            {"original_source_pts", entry.at("source_pts")},
            {"source_time_base", {source.timebase().num, source.timebase().den}},
            {"clip_ordinal", ordinal}, {"present_index", entry.at("present_index")},
            {"ti_row_index", entry.at("ti_row_index")}, {"output_pts", processed},
            {"output_time_base", {1, 30}}, {"pixel_format", "vulkan/nv12"}};
        ledger << identity.dump() << "\n";
        if (!ledger) throw std::runtime_error("Identity ledger write failed");
        av_frame_unref(input.get());
        av_frame_unref(output.get());
        if (processed + 1 == options.frames) { ++processed; break; }
        if (!source.next(input.get())) {
            if (expectedOrdinal != scene.manifest.at("frames").size())
                throw std::runtime_error("Source ended before complete scene coverage");
            if (!options.loop) { ++processed; break; }
            source.rewind();
            ++loops;
            expectedOrdinal = 0;
            if (!source.next(input.get())) throw std::runtime_error("Empty source after loop");
        }
        allocateOutput();
    }
    sink.finish();
    ledger.close();
    double seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - start).count();
    std::cout << nlohmann::json{{"frames", processed}, {"with_ti", withTi}, {"loops", loops},
                              {"seconds", seconds}, {"fps", processed / seconds}, {"pts_mismatches", 0},
                              {"decode", "vulkan/nv12"}, {"render", "Dawn/Vulkan opaque-FD shared images"},
                              {"encode", "h264_vulkan"}, {"cpu_video_transfers", 0}}.dump() << "\n";
    return 0;
} catch (const std::exception& e) {
    std::cerr << "Fatal: " << e.what() << "\n";
    return 1;
}
