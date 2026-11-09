#include "renderer.hpp"
#include "shader.hpp"

Renderer::Renderer(Bridge& b, const Scene& scene) : bridge(b) {
    auto buffer = [&](const void* data, size_t size, wgpu::BufferUsage usage) {
        wgpu::BufferDescriptor descriptor;
        descriptor.size = size;
        descriptor.usage = usage | wgpu::BufferUsage::CopyDst;
        auto result = bridge.dawnDevice.CreateBuffer(&descriptor);
        if (data) bridge.dawnDevice.GetQueue().WriteBuffer(result, 0, data, size);
        return result;
    };
    parameters = buffer(nullptr, sizeof(Parameters), wgpu::BufferUsage::Uniform);
    artwork = buffer(scene.pixels.data(), scene.pixels.size() * sizeof(scene.pixels[0]), wgpu::BufferUsage::Storage);
    mips = buffer(scene.levels.data(), scene.levels.size() * sizeof(Mip), wgpu::BufferUsage::Storage);
    wgpu::TextureDescriptor maskDescriptor;
    maskDescriptor.size = {scene.width, scene.height, 1};
    maskDescriptor.format = wgpu::TextureFormat::R8Unorm;
    maskDescriptor.usage = wgpu::TextureUsage::TextureBinding | wgpu::TextureUsage::CopyDst;
    mask = bridge.dawnDevice.CreateTexture(&maskDescriptor);
    wgpu::ShaderSourceWGSL wgsl;
    wgsl.code = sceneShader;
    wgpu::ShaderModuleDescriptor shaderDesc;
    shaderDesc.nextInChain = &wgsl;
    auto shader = bridge.dawnDevice.CreateShaderModule(&shaderDesc);
    wgpu::ComputePipelineDescriptor descriptor;
    descriptor.compute.module = shader;
    descriptor.compute.entryPoint = "main";
    pipeline = bridge.dawnDevice.CreateComputePipeline(&descriptor);
    std::array<wgpu::BindGroupEntry, 7> entries;
    for (unsigned i = 0; i < 3; ++i) {
        entries[i].binding = i;
        entries[i].textureView = bridge.images[i].view;
    }
    entries[3].binding = 3; entries[3].buffer = parameters;
    entries[4].binding = 4; entries[4].buffer = artwork;
    entries[5].binding = 5; entries[5].buffer = mips;
    entries[6].binding = 6; entries[6].textureView = mask.CreateView();
    wgpu::BindGroupDescriptor bind;
    bind.layout = pipeline.GetBindGroupLayout(0);
    bind.entryCount = entries.size();
    bind.entries = entries.data();
    bindings = bridge.dawnDevice.CreateBindGroup(&bind);
}

void Renderer::render(const Parameters& data, const std::vector<uint8_t>& maskData) {
    bridge.dawnDevice.GetQueue().WriteBuffer(parameters, 0, &data, sizeof(data));
    wgpu::TexelCopyTextureInfo destination;
    destination.texture = mask;
    wgpu::TexelCopyBufferLayout layout;
    layout.bytesPerRow = bridge.width;
    layout.rowsPerImage = bridge.height;
    wgpu::Extent3D size{bridge.width, bridge.height, 1};
    bridge.dawnDevice.GetQueue().WriteTexture(&destination, maskData.data(), maskData.size(), &layout, &size);
    auto encoder = bridge.dawnDevice.CreateCommandEncoder();
    auto pass = encoder.BeginComputePass();
    pass.SetPipeline(pipeline);
    pass.SetBindGroup(0, bindings);
    pass.DispatchWorkgroups((bridge.width / 4 + 7) / 8, (bridge.height / 2 + 7) / 8);
    pass.End();
    auto commands = encoder.Finish();
    bridge.dawnDevice.GetQueue().Submit(1, &commands);
}
