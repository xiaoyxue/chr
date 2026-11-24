# glTF support

**Status:** `implemented` for static GLB assets (#208). `chr_renderer::glb` imports
them; the shared PBR path in `mesh.wgsl` shades them. This page lists what the
importer reads, what it ignores and what it rejects. Every material extension
follows its Khronos definition, layered on chr's metallic-roughness base (GGX,
height-correlated Smith visibility, Burley diffuse). Image-based light keeps the
equirectangular panorama and its GGX-prefiltered roughness layers; no cubemaps are
involved.

## Policy

- An extension in `extensionsRequired` that chr does not implement rejects the file
  (`unsupported GLB feature: required extension …`).
- Any other unimplemented extension in `extensionsUsed` is ignored: the asset loads
  and renders without it, and its warnings name the extension. `chr-server` logs them
  when it loads a render description.
- Core features chr does not implement (below) reject the file, except animation
  clips, which are ignored with a warning.

## Core glTF 2.0

| Feature | Support |
| --- | --- |
| GLB with one embedded BIN buffer | Supported. External buffers or images (URIs) are rejected; the importer has no filesystem access |
| Scene graph | The default scene (or the first one), with node TRS/matrix transforms baked into the geometry once. glTF +Y-up becomes chr -Z-up: `(x, y, z) -> (x, z, -y)`. Mirrored nodes keep their winding |
| Triangles (mode 4), indexed or not | Supported. Points, lines, strips and fans are rejected |
| `POSITION`, `NORMAL`, `TANGENT`, `TEXCOORD_0`, `COLOR_0` | Supported, including sparse accessors. Missing normals become flat face normals. Missing tangents are derived for materials that need a tangent frame (normal, clearcoat-normal or anisotropy maps) |
| `TEXCOORD_1` and further sets | Rejected when a texture uses them (`texCoord` other than 0) |
| Metallic-roughness material | Base colour, metallic, roughness, normal (scale), occlusion (strength), emissive factors and textures; `doubleSided` |
| `alphaMode` | `OPAQUE`, `MASK` (cutoff). `BLEND` is rejected unless the caller opts in with `ImportOptions::blend_as_mask` (render descriptions: `blend_as: "mask"`), which cuts it out at 0.5; the renderer has no translucent pass |
| Images | Embedded PNG and JPEG; WebP with `EXT_texture_webp`. Decoded to RGBA8 and shared between materials |
| Samplers | Wrap modes and nearest/linear/mipmap filters. Trilinear samplers also filter anisotropically (16x) |
| Skins, morph targets | Rejected |
| Animations | Ignored with a warning; the authored default pose is rendered |
| Cameras, lights in the file | Ignored; scene lights and the camera come from chr |

## Extensions

| Extension | Support |
| --- | --- |
| `KHR_materials_emissive_strength` | Supported |
| `KHR_materials_unlit` | Supported: base colour and emission, no lighting |
| `KHR_materials_ior` | Supported, including `ior = 0` (Fresnel 1 at every angle, the specular-glossiness compatibility mode) |
| `KHR_materials_specular` | Supported: strength (texture A) and colour (texture RGB, sRGB); the reflectance is clamped to 1 and the diffuse keeps the energy of the strongest Fresnel channel |
| `KHR_materials_clearcoat` | Supported: intensity (R), roughness (G) and an optional coat normal map. The coat's Fresnel at N·V dims everything beneath it, emission included. Its GGX lobe has F0 0.04 |
| `KHR_materials_sheen` | Supported: colour (RGB, sRGB) and roughness (A). Charlie distribution and visibility (Conty and Kulla 2017) with albedo scaling at N·V. The directional albedo is baked into the B channel of the BRDF lookup table. Under IBL, sheen reads the GGX-prefiltered panorama around the normal. Sheen roughness below 0.07 shades as 0.07 |
| `KHR_materials_anisotropy` | Supported: strength, rotation and direction texture (RG direction, B strength). Anisotropic GGX D and visibility for punctual lights; IBL samples along the bent normal of the extension's reference implementation |
| `KHR_materials_iridescence` | Supported: intensity (R), film IOR, thickness range and texture (G). Belcour and Barla thin-film Fresnel, evaluated at N·V for every light, as the Khronos sample viewer does. A film of zero thickness is no film |
| `KHR_texture_transform` | Supported on every texture slot (offset, rotation, scale). A positive rotation turns the coordinates as in the extension's worked example (the Khronos sample viewer and three.js agree). Normal-map and anisotropy directions follow the transform's rotation and mirroring. A `texCoord` override other than 0 is rejected |
| `KHR_materials_pbrSpecularGlossiness` | Converted at import, as `KHR_materials_specular` recommends: diffuse becomes the base colour, specular the specular colour with `ior = 0`, and `1 - glossiness` the roughness (baked into a roughness texture when glossiness is textured) |
| `EXT_texture_webp` | Supported, also as a texture's only source |
| `KHR_mesh_quantization` | Supported: integer and normalized positions, normals, tangents and texture coordinates |
| `KHR_materials_variants` | The default material of each primitive is used; variants are not selectable |
| `KHR_materials_transmission`, `KHR_materials_volume`, `KHR_materials_dispersion` | Not implemented: they need a translucent, refracting pass that composites over the camera picture. Ignored with a warning when optional |
| `KHR_texture_basisu`, `KHR_draco_mesh_compression`, `EXT_meshopt_compression` | Not implemented: they need native decoders. Files that require them are rejected; an optional `KHR_texture_basisu` falls back to the texture's PNG/JPEG source |
| `KHR_lights_punctual`, `KHR_animation_pointer`, `KHR_xmp_json_ld` and others | Ignored with a warning (rejected when required) |

## Shading limits

- Area lights (`ltc-plausible-v1`) shade the metallic-roughness base layer only,
  dimmed by the clearcoat and sheen; their own lobes, specular colour, IOR,
  anisotropy and iridescence do not apply to area lights.
- Shadow catchers, decals and flags keep their own materials; extension layers are
  part of imported GLB materials. A render description's `material_overrides`
  replace the core factors and keep the asset's extension layers.
- The device requests 32 sampled textures and 32 samplers per shader stage
  (`Gpu::required_limits`) for the 15 material maps beside the lighting textures;
  desktop Vulkan and D3D12 adapters offer far more.

## Verification

- `server/renderer/tests/glb.rs`: importer tests for each extension's factors,
  textures and transforms, the required/optional policy, specular-glossiness
  conversion, quantized attributes, WebP and invalid values.
- `server/renderer/tests/golden_gpu.rs` (GPU, ignored by default): golden images in
  `server/renderer/tests/golden/` compared within 2 code values (0.2 % outliers).
  The `base_*` goldens cover the features that predate the extensions (punctual
  lights and shadows, texture maps, alpha masks, IBL, area lights, decals, shadow
  catchers, flags); they were rendered by `main` before the extensions and the
  extension change reproduces them byte for byte. The `extension_*` goldens cover
  each extension over direct light and IBL. Behaviour tests in the same file check
  each layer independently of the goldens. Regenerate goldens only after an
  intended change: `CHR_UPDATE_GOLDEN=1 cargo test -p chr-renderer --test
  golden_gpu -- --ignored`; a failing comparison writes the actual image to
  `output/renderer/golden/`.
