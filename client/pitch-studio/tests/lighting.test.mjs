import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

const server = await createServer({ configFile: false, server: { middlewareMode: true } });
after(() => server.close());
const { DEFAULT_PRESET, ORLANDO_PRESET, availablePresets, builtInPresets, describeLighting, emptyLibrary, selectedPreset, validateLightingLibrary } =
  await server.ssrLoadModule('/src/lighting/lighting.ts');
const { describeScene, environmentAssetId } = await server.ssrLoadModule('/src/render/render-description.ts');

const field = { length: 105, width: 68 };
const close = (actual, expected, tolerance = 1e-9) =>
  actual.forEach((value, i) => assert.ok(Math.abs(value - expected[i]) <= tolerance, `${actual} vs ${expected}`));

test("the built-in default preset is chr-server's default rig lit by the fly-studio panorama", () => {
  const [preset] = builtInPresets(field, '/srv/ibl');
  assert.equal(preset.id, DEFAULT_PRESET);
  assert.equal(preset.environment.path, '/srv/ibl/fly-studio-03_4K.exr');
  const lighting = describeLighting(preset, 'IBL-fly');
  assert.deepEqual(lighting.environment, { asset_ref: 'IBL-fly', gain: 0.5, rotation: 0, visible: false });
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
  // Switched off, the panorama and the fill keep their values but are not sent.
  const off = structuredClone(orlando);
  off.environment.enabled = false;
  off.constant.enabled = false;
  const dark = describeLighting(off);
  assert.equal(dark.environment, null);
  assert.equal(dark.constant, undefined);
});

test("an operator's edit of a built-in replaces it, keeps its ID and can be dropped to reset it", () => {
  const builtIns = builtInPresets(field, '/srv/ibl');
  const edit = structuredClone(builtIns.find((preset) => preset.id === ORLANDO_PRESET));
  delete edit.builtIn;
  edit.environment.gain = 0.2;
  edit.lights[0].enabled = false;
  const library = validateLightingLibrary({ ...emptyLibrary(), presets: [edit], selected: { 'MATCH-1': ORLANDO_PRESET } });
  const offered = availablePresets(library, builtIns);
  assert.deepEqual(offered.map((preset) => [preset.id, Boolean(preset.edited)]), [[DEFAULT_PRESET, false], [ORLANDO_PRESET, true]]);
  const used = selectedPreset(library, builtIns, 'MATCH-1');
  assert.equal(used.environment.gain, 0.2);
  assert.equal(describeLighting(used, 'IBL-x').lights.length, 4);
  assert.equal(selectedPreset({ ...library, presets: [] }, builtIns, 'MATCH-1').environment.gain, 0.5);
  assert.throws(() => validateLightingLibrary({ ...library, presets: [{ ...edit, id: 'builtin-other' }] }));
});

test('operator presets: disabled lights, shadows off, the light limit and the stored document', () => {
  const [base] = builtInPresets(field, '/srv/ibl');
  const preset = { ...structuredClone(base), id: 'preset-1', name: 'Night' };
  delete preset.builtIn;
  preset.lights[0].enabled = false;
  preset.lights[1].shadows = false;
  preset.lights.push({ kind: 'point', id: 'lamp', name: 'Lamp', enabled: true, shadows: false, color: [1, 0.5, 0.25],
    position: [0, 30, 12], intensity: 800, radius: 0.3 });
  const lighting = describeLighting(preset, 'IBL-fly');
  assert.equal(lighting.lights.length, 5);
  assert.equal(lighting.lights[0].shadow_mode, 'off');
  assert.deepEqual(lighting.lights.at(-1), { light_id: 'point-lamp', shadow_mode: 'off', kind: 'point', position: [0, 30, -12],
    intensity: [800, 400, 200], source_radius: 0.3 });
  const crowded = structuredClone(preset);
  for (let i = 0; i < 4; i += 1) crowded.lights.push({ ...crowded.lights.at(-1), id: `lamp-${i}` });
  assert.throws(() => describeLighting(crowded, 'IBL-fly'), /最多/);

  const library = { ...emptyLibrary(), presets: [preset], selected: { 'MATCH-1': 'preset-1' } };
  assert.equal(validateLightingLibrary(library), library);
  assert.equal(selectedPreset(library, builtInPresets(field, '/x'), 'MATCH-1').name, 'Night');
  assert.equal(selectedPreset(library, builtInPresets(field, '/x'), 'MATCH-2').id, DEFAULT_PRESET);
  for (const broken of [
    { ...preset, id: 'builtin-unknown' },
    { ...preset, lights: preset.lights.map((light) => light.kind === 'spot' ? { ...light, inner: 50, outer: 40 } : light) },
    { ...preset, environment: { enabled: true, path: 'relative.exr', gain: 1, rotation: 0 } },
    { ...preset, environment: { enabled: true, path: null, gain: 1, rotation: 0 } },
    { ...preset, constant: { enabled: true, intensity: 1, color: [0, 0, 0] } },
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
