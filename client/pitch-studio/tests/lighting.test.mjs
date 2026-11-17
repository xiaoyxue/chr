import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { DEFAULT_PRESET, ORLANDO_PRESET, builtInPresets, describeLighting, emptyLibrary, selectedPreset, validateLightingLibrary } =
  await server.ssrLoadModule('/src/lighting/lighting.ts');
const { describeScene, environmentAssetId } = await server.ssrLoadModule('/src/render/render-description.ts');

const field = { length: 105, width: 68 };
const close = (actual, expected, tolerance = 1e-9) =>
  actual.forEach((value, i) => assert.ok(Math.abs(value - expected[i]) <= tolerance, `${actual} vs ${expected}`));

test("the built-in default preset is chr-server's default rig", () => {
  const [preset] = builtInPresets(field, '/srv/ibl');
  assert.equal(preset.id, DEFAULT_PRESET);
  const lighting = describeLighting(preset);
  assert.equal(lighting.environment, null);
  assert.equal(lighting.constant, undefined);
  const spots = lighting.lights.filter((light) => light.kind === 'spot');
  assert.deepEqual(spots.map((spot) => spot.position), [[-52.5, -34, -35], [-52.5, 34, -35], [52.5, -34, -35], [52.5, 34, -35]]);
  close(spots[0].direction, [52.5, 34, 35]);
  close(spots[0].intensity, [16000, 16800, 20000], 1e-6);
  close([spots[0].inner_angle, spots[0].outer_angle, spots[0].source_radius], [0.4, 0.9, 0.5]);
  assert.equal(spots[0].shadow_mode, null);
  const sun = lighting.lights.find((light) => light.kind === 'directional');
  const length = Math.hypot(0.5, 0.6, 1);
  close(sun.direction, [0.5 / length, 0.6 / length, 1 / length]);
  close(sun.irradiance, [2.5, 2.35, 2.1], 1e-9);
});

test('the Orlando preset adds its panorama and a constant fill', () => {
  const orlando = builtInPresets(field, '/srv/ibl/').find((preset) => preset.id === ORLANDO_PRESET);
  assert.equal(orlando.environment.path, '/srv/ibl/orlando_stadium_4k.exr');
  assert.throws(() => describeLighting(orlando), /素材/);
  const lighting = describeLighting(orlando, 'IBL-abc');
  assert.deepEqual(lighting.environment, { asset_ref: 'IBL-abc', gain: 0.5, rotation: 0, visible: false });
  assert.deepEqual(lighting.constant, [0.1, 0.1, 0.1]);
  assert.equal(lighting.lights.length, 5);
});

test('operator presets: disabled lights, shadows off, the light limit and the stored document', () => {
  const [base] = builtInPresets(field, '/srv/ibl');
  const preset = { ...structuredClone(base), id: 'preset-1', name: 'Night' };
  delete preset.builtIn;
  preset.lights[0].enabled = false;
  preset.lights[1].shadows = false;
  preset.lights.push({ kind: 'point', id: 'lamp', name: 'Lamp', enabled: true, shadows: false, color: [1, 0.5, 0.25],
    position: [0, 30, 12], intensity: 800, radius: 0.3 });
  const lighting = describeLighting(preset);
  assert.equal(lighting.lights.length, 5);
  assert.equal(lighting.lights[0].shadow_mode, 'off');
  assert.deepEqual(lighting.lights.at(-1), { light_id: 'point-lamp', shadow_mode: 'off', kind: 'point', position: [0, 30, -12],
    intensity: [800, 400, 200], source_radius: 0.3 });
  const crowded = structuredClone(preset);
  for (let i = 0; i < 4; i += 1) crowded.lights.push({ ...crowded.lights.at(-1), id: `lamp-${i}` });
  assert.throws(() => describeLighting(crowded), /最多/);

  const library = { ...emptyLibrary(), presets: [preset], selected: { 'MATCH-1': 'preset-1' } };
  assert.equal(validateLightingLibrary(library), library);
  assert.equal(selectedPreset(library, builtInPresets(field, '/x'), 'MATCH-1').name, 'Night');
  assert.equal(selectedPreset(library, builtInPresets(field, '/x'), 'MATCH-2').id, DEFAULT_PRESET);
  for (const broken of [
    { ...preset, id: DEFAULT_PRESET },
    { ...preset, lights: preset.lights.map((light) => light.kind === 'spot' ? { ...light, inner: 50, outer: 40 } : light) },
    { ...preset, environment: { path: 'relative.exr', gain: 1, rotation: 0 } },
    { ...preset, constant: { intensity: 1, color: [0, 0, 0] } },
  ]) assert.throws(() => validateLightingLibrary({ ...library, presets: [broken] }));
});

test('a described scene carries the lighting and its panorama as an environment asset', () => {
  const orlando = builtInPresets(field, '/srv/ibl').find((preset) => preset.id === ORLANDO_PRESET);
  const sha256 = 'ab'.repeat(32), id = environmentAssetId(sha256);
  const scene = describeScene({ field, slots: [], hidden: new Set(), geometry: new Map(), assets: new Map(), packagePath: '',
    lighting: { lighting: describeLighting(orlando, id), environment: { id, path: orlando.environment.path, sha256 } } });
  assert.deepEqual(scene.assets, [{ id: 'IBL-abababababababab', kind: 'environment', source: { path: '/srv/ibl/orlando_stadium_4k.exr' }, sha256 }]);
  assert.equal(scene.lighting.environment.asset_ref, id);
  assert.equal(describeScene({ field, slots: [], hidden: new Set(), geometry: new Map(), assets: new Map(), packagePath: '' }).lighting, undefined);
});
