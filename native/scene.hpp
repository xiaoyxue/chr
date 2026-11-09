#pragma once
#include "media.hpp"
#include <nlohmann/json.hpp>
#include <fstream>
#include <vector>
#include <array>

struct Quad {
    std::array<float, 4> r0{}, r1{}, r2{};
    std::array<int32_t, 4> bounds{};
    std::array<uint32_t, 4> meta{};
};
struct Parameters {
    std::array<float, 4> color{0.299f, 0.114f, 0.5f, 0.0f};
    std::array<uint32_t, 4> control{};
    std::array<Quad, 6> quads{};
};
struct Mip {
    uint32_t width, height, offset, unused = 0;
};

class Scene {
    std::ifstream masks;
public:
    nlohmann::json manifest;
    std::vector<std::array<float, 4>> pixels;
    std::vector<Mip> levels;
    std::vector<std::pair<uint32_t, uint32_t>> textures;
    uint32_t width, height;
    explicit Scene(const std::string& path);
    size_t associate(int64_t pts, AVRational timebase) const;
    Parameters parameters(size_t ordinal, const AVFrame* frame, uint32_t mode) const;
    std::vector<uint8_t> mask(size_t ordinal);
    void verifyVideo() const;
};

std::string timecode(uint64_t sequence);
