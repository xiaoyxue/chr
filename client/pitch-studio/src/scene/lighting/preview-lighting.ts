import * as THREE from 'three';

export function createPreviewLighting() {
  const group = new THREE.Group();
  const ambient = new THREE.HemisphereLight('#cbd6ef', '#486044', 0.8);
  ambient.position.set(0, 0, -1);
  group.add(ambient);
  const lights = [[-62.5, -44], [62.5, -44], [-62.5, 44], [62.5, 44]].map(([x, y]) => {
    const light = new THREE.SpotLight('#fff8eb', 20000, 300, Math.PI / 3, 0.3, 2);
    light.position.set(x, y, -35);
    light.target.position.set(0, 0, 0);
    light.castShadow = true;
    light.shadow.mapSize.set(2048, 2048);
    light.shadow.camera.up.set(0, 0, -1);
    light.shadow.camera.near = 1;
    light.shadow.camera.far = 300;
    light.shadow.focus = 0.3;
    light.shadow.bias = -0.00002;
    light.shadow.normalBias = 0.015;
    light.shadow.radius = 2;
    light.shadow.autoUpdate = false;
    group.add(light, light.target);
    return light;
  });
  return {
    group,
    markShadows: () => lights.forEach((light) => { light.shadow.needsUpdate = true; }),
    dispose: () => lights.forEach((light) => { light.shadow.dispose(); light.dispose(); }),
  };
}
