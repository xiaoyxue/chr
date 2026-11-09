#pragma once
#include "bridge.hpp"
#include "scene.hpp"
#include <string>

class Renderer {
    Bridge& bridge;
    wgpu::ComputePipeline pipeline;
    wgpu::BindGroup bindings;
    wgpu::Buffer parameters, artwork, mips;
    wgpu::Texture mask;
public:
    Renderer(Bridge& bridge, const Scene& scene);
    void render(const Parameters& data, const std::vector<uint8_t>& maskData);
};
