#include "scene.hpp"
#include <filesystem>
#include <iostream>

static void require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}
template<class F> void rejects(F action, const char* message) {
    bool rejected = false;
    try { action(); } catch (const std::exception&) { rejected = true; }
    require(rejected, message);
}

int main() try {
    AVCodecParameters header{};
    uint8_t extradata = 1;
    header.codec_type = AVMEDIA_TYPE_VIDEO;
    header.codec_id = AV_CODEC_ID_H264;
    header.width = 1212;
    header.height = 654;
    header.extradata = &extradata;
    header.extradata_size = 1;
    requireCompleteVideoHeader(header, {1, 15360});
    header.extradata_size = 0;
    rejects([&] { requireCompleteVideoHeader(header, {1, 15360}); }, "Missing extradata triggered no rejection");
    header.extradata_size = 1;
    header.width = 0;
    rejects([&] { requireCompleteVideoHeader(header, {1, 15360}); }, "Missing raster triggered no rejection");
    header.width = 1212;
    rejects([&] { requireCompleteVideoHeader(header, {0, 15360}); }, "Missing time base triggered no rejection");
    auto directory = std::filesystem::path("scene-test-data");
    std::filesystem::create_directory(directory);
    auto path = directory / "scene.json";
    auto mask = directory / "masks.r8";
    {
        std::ofstream file(mask, std::ios::binary);
        std::array<uint8_t, 24> bytes{};
        std::fill(bytes.begin() + 8, bytes.begin() + 16, 255);
        file.write(reinterpret_cast<char*>(bytes.data()), bytes.size());
    }
    nlohmann::json frames = nlohmann::json::array();
    for (int i = 0; i < 3; ++i) {
        nlohmann::json quads = nlohmann::json::array();
        for (int q = 0; q < 6; ++q) quads.push_back({{"valid", false}, {"texture", 0}});
        frames.push_back({{"clip_pts", i * 512}, {"time_base_num", 1}, {"time_base_den", 15360},
                          {"quads", quads}, {"ti_row_index", i == 1 ? nlohmann::json(0) : nlohmann::json(nullptr)}});
    }
    nlohmann::json manifest{{"version", 1}, {"width", 4}, {"height", 2}, {"fps_num", 30}, {"fps_den", 1},
                            {"opacity", 0.5}, {"textures", nlohmann::json::array()}, {"frames", frames},
                            {"mask_path", mask.string()}};
    { std::ofstream file(path); file << manifest; }
    Scene scene(path.string());
    require(scene.associate(3000, {1, 90000}) == 1, "Equivalent rational PTS did not match Ti");
    rejects([&] { scene.associate(3001, {1, 90000}); }, "Rounded PTS incorrectly matched scene");
    rejects([&] { scene.associate(AV_NOPTS_VALUE, {1, 90000}); }, "Missing PTS was accepted");
    require(scene.mask(1)[0] == 255 && scene.mask(0)[0] == 0, "Mask association or seek is stale");
    rejects([&] { scene.mask(3); }, "Missing mask did not fail");
    AVFrame frame{};
    frame.colorspace = AVCOL_SPC_UNSPECIFIED;
    require(scene.parameters(0, &frame, 0).control[1] == 0, "Boundary frame invented Ti");
    require(scene.parameters(1, &frame, 0).control[1] == 1, "Valid Ti disappeared");
    require(scene.associate(0, {1, 15360}) == 0 && timecode(1482) == "00:00:49:12",
            "Source-loop identity must remain independent of continuous timecode");
    require(timecode(2592000) == "00:00:00:00", "24-hour NDF wrap failed");
    manifest["frames"][1]["clip_pts"] = 513;
    { std::ofstream file(path); file << manifest; }
    rejects([&] { Scene broken(path.string()); }, "Non-CFR scene was accepted");
    std::filesystem::remove_all(directory);
    std::cout << "Exact PTS, missing Ti, masks, and loop timecode tests passed\n";
    return 0;
} catch (const std::exception& error) {
    std::cerr << error.what() << "\n";
    return 1;
}
