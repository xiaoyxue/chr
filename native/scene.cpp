#include "scene.hpp"
extern "C" {
#include <libavutil/sha.h>
}
#include <algorithm>
#include <cmath>
#include <iomanip>
#include <sstream>

Scene::Scene(const std::string& path) {
    std::ifstream input(path);
    manifest = nlohmann::json::parse(input);
    width = manifest.at("width");
    height = manifest.at("height");
    if (manifest.at("version") != 1 || manifest.at("fps_num") != 30 || manifest.at("fps_den") != 1 ||
        manifest.at("opacity") != 0.5 || width % 4 || height % 2 || !width || !height)
        throw std::runtime_error("Unsupported scene version, raster, opacity, or simulation rate");
    const auto& frames = manifest.at("frames");
    if (frames.empty()) throw std::runtime_error("Scene contains no frames");
    for (size_t i = 0; i < frames.size(); ++i) {
        const auto& frame = frames[i];
        AVRational tb{frame.at("time_base_num"), frame.at("time_base_den")};
        if (tb.num <= 0 || tb.den <= 0 || av_compare_ts(frame.at("clip_pts"), tb, i, {1, 30}) != 0 ||
            frame.at("quads").size() != 6)
            throw std::runtime_error("Scene frame identity is not exact 30 fps");
    }
    for (const auto& texture : manifest.at("textures")) {
        uint32_t base = levels.size();
        std::ifstream file(texture.at("path").get<std::string>(), std::ios::binary);
        if (!file) throw std::runtime_error("Cannot open private logo mipmaps");
        for (const auto& level : texture.at("levels")) {
            uint32_t w = level.at("width"), h = level.at("height");
            if (!w || !h || uint64_t(w) * h > 16777216) throw std::runtime_error("Invalid logo mip dimensions");
            Mip m{w, h, static_cast<uint32_t>(pixels.size())};
            levels.push_back(m);
            pixels.resize(pixels.size() + uint64_t(w) * h);
            file.seekg(level.at("offset").get<uint64_t>());
            file.read(reinterpret_cast<char*>(pixels.data() + m.offset), uint64_t(w) * h * 16);
            if (!file) throw std::runtime_error("Truncated logo mipmap");
        }
        textures.emplace_back(base, levels.size() - base);
    }
    masks.open(manifest.at("mask_path").get<std::string>(), std::ios::binary);
    if (!masks) throw std::runtime_error("Cannot open private mask cache");
}

void Scene::verifyVideo() const {
    std::ifstream input(manifest.at("video").get<std::string>(), std::ios::binary);
    if (!input) throw std::runtime_error("Cannot open compressed source for identity check");
    AVSHA* sha = av_sha_alloc();
    avcheck(av_sha_init(sha, 256), "Initialize source hash");
    std::array<uint8_t, 65536> buffer;
    while (input) {
        input.read(reinterpret_cast<char*>(buffer.data()), buffer.size());
        av_sha_update(sha, buffer.data(), input.gcount());
    }
    std::array<uint8_t, 32> digest;
    av_sha_final(sha, digest.data());
    av_free(sha);
    std::ostringstream hex;
    for (auto byte : digest) hex << std::hex << std::setw(2) << std::setfill('0') << unsigned(byte);
    if (hex.str() != manifest.at("pins").at("video_sha256").get<std::string>())
        throw std::runtime_error("Source SHA256 does not match prepared camera/mask identity");
}

size_t Scene::associate(int64_t pts, AVRational timebase) const {
    if (pts == AV_NOPTS_VALUE) throw std::runtime_error("Decoded frame has no source PTS");
    const auto& frames = manifest.at("frames");
    auto iterator = std::lower_bound(frames.begin(), frames.end(), pts, [&](const auto& frame, int64_t value) {
        return av_compare_ts(frame.at("clip_pts"), {frame.at("time_base_num"), frame.at("time_base_den")}, value, timebase) < 0;
    });
    if (iterator == frames.end() ||
        av_compare_ts(iterator->at("clip_pts"), {iterator->at("time_base_num"), iterator->at("time_base_den")}, pts, timebase))
        throw std::runtime_error("Source PTS has no exact scene association");
    return iterator - frames.begin();
}

Parameters Scene::parameters(size_t ordinal, const AVFrame* frame, uint32_t mode) const {
    Parameters data;
    if (frame->colorspace == AVCOL_SPC_BT709) {
        data.color[0] = 0.2126f;
        data.color[1] = 0.0722f;
    } else if (frame->colorspace != AVCOL_SPC_UNSPECIFIED && frame->colorspace != AVCOL_SPC_SMPTE170M &&
               frame->colorspace != AVCOL_SPC_BT470BG)
        throw std::runtime_error("Unsupported YUV color matrix; no silent color conversion");
    data.color[3] = frame->color_range == AVCOL_RANGE_JPEG ? 1.f : 0.f;
    data.control[0] = mode;
    const auto& entry = manifest.at("frames").at(ordinal);
    if (entry.at("ti_row_index").is_null()) return data;
    data.control[1] = 1;
    for (size_t i = 0; i < 6; ++i) {
        const auto& q = entry.at("quads").at(i);
        auto& dst = data.quads[i];
        if (!q.at("valid").get<bool>()) continue;
        auto [base, count] = textures.at(q.at("texture").get<size_t>());
        dst.meta = {1, base, count, 0};
        for (int j = 0; j < 3; ++j) {
            dst.r0[j] = q.at("inverse").at(j);
            dst.r1[j] = q.at("inverse").at(j + 3);
            dst.r2[j] = q.at("inverse").at(j + 6);
            if (!std::isfinite(dst.r0[j]) || !std::isfinite(dst.r1[j]) || !std::isfinite(dst.r2[j]))
                throw std::runtime_error("Non-finite homography");
        }
        for (int j = 0; j < 4; ++j) dst.bounds[j] = q.at("bounds").at(j);
    }
    return data;
}

std::vector<uint8_t> Scene::mask(size_t ordinal) {
    std::vector<uint8_t> result(uint64_t(width) * height);
    masks.seekg(uint64_t(ordinal) * result.size());
    masks.read(reinterpret_cast<char*>(result.data()), result.size());
    if (!masks) throw std::runtime_error("Missing same-frame mask");
    return result;
}

std::string timecode(uint64_t sequence) {
    sequence %= 24 * 60 * 60 * 30;
    std::ostringstream out;
    out << std::setfill('0') << std::setw(2) << sequence / 108000 << ":"
        << std::setw(2) << sequence / 1800 % 60 << ":" << std::setw(2) << sequence / 30 % 60 << ":"
        << std::setw(2) << sequence % 30;
    return out.str();
}
