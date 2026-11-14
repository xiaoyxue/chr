import * as THREE from 'three';
import { applyAuxiliaryCamera } from '../scene/cameras/result-camera.ts';
import type { DiagnosticProjection, ResultFrame, ResultShot } from './bundle-result.ts';

function extrapolatedSupport(projection: DiagnosticProjection, bounds: THREE.Box3) {
  const support: [number, number, number, number] = [...projection.radial.support];
  if (bounds.isEmpty()) return support;
  const p = projection.matrix, depths: number[] = [];
  for (const x of [bounds.min.x, bounds.max.x]) for (const y of [bounds.min.y, bounds.max.y]) for (const z of [bounds.min.z, bounds.max.z]) {
    const depth = p[8] * x + p[9] * y + p[10] * z + p[11];
    depths.push(depth);
    if (Math.abs(depth) < 1e-8) throw new Error('点云广告过于接近投影相机平面，请调整广告位或关闭实验性外推。');
    const px = (p[0] * x + p[1] * y + p[2] * z + p[3]) / depth;
    const py = (p[4] * x + p[5] * y + p[6] * z + p[7]) / depth;
    if (!Number.isFinite(px) || !Number.isFinite(py)) throw new Error('点云广告外推范围无效，请检查广告位坐标。');
    support[0] = Math.min(support[0], Math.floor(px - 1));
    support[1] = Math.min(support[1], Math.floor(py - 1));
    support[2] = Math.max(support[2], Math.ceil(px + 1));
    support[3] = Math.max(support[3], Math.ceil(py + 1));
  }
  if (Math.min(...depths) < 0 && Math.max(...depths) > 0) throw new Error('点云广告跨越投影相机平面，无法稳定外推；请调整广告位或关闭实验性外推。');
  const { radial } = projection;
  if (radial.k1 < 0) {
    const radius = radial.scale * Math.sqrt(-1 / (3 * radial.k1));
    support[0] = Math.max(support[0], Math.floor(radial.centre[0] - radius));
    support[1] = Math.max(support[1], Math.floor(radial.centre[1] - radius));
    support[2] = Math.min(support[2], Math.ceil(radial.centre[0] + radius));
    support[3] = Math.min(support[3], Math.ceil(radial.centre[1] + radius));
  }
  return support;
}

export class RadialResultPass {
  private readonly target = new THREE.WebGLRenderTarget(1, 1, { depthBuffer: true });
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);
  private readonly material = new THREE.ShaderMaterial({
    depthTest: false, depthWrite: false, transparent: true, premultipliedAlpha: true,
    uniforms: {
      image: { value: this.target.texture },
      inversePostwarp: { value: new THREE.Matrix3() },
      resolution: { value: new THREE.Vector2() },
      centre: { value: new THREE.Vector2() },
      support: { value: new THREE.Vector4() },
      scale: { value: 1 }, k1: { value: 0 }, maximumRadius: { value: 1 },
    },
    vertexShader: `
      void main() { gl_Position = vec4(position.xy, 0.0, 1.0); }
    `,
    fragmentShader: `
      uniform sampler2D image;
      uniform mat3 inversePostwarp;
      uniform vec2 resolution, centre;
      uniform vec4 support;
      uniform float scale, k1, maximumRadius;
      void main() {
        vec2 nativePixel = vec2(gl_FragCoord.x - 0.5, resolution.y - gl_FragCoord.y - 0.5);
        vec2 snc = (2.0 * nativePixel - resolution) / sqrt(resolution.x * resolution.y);
        vec3 reference = inversePostwarp * vec3(snc, 1.0);
        if (abs(reference.z) < 1e-8) discard;
        vec2 distorted = (reference.xy / reference.z - centre) / scale;
        float distance = length(distorted);
        float limit = maximumRadius * (1.0 + k1 * maximumRadius * maximumRadius);
        if (distance > limit) discard;
        // Invert only the centre-continuous monotonic branch, never negate k1.
        float low = 0.0, high = maximumRadius;
        for (int i = 0; i < 28; i++) {
          float middle = (low + high) * 0.5;
          if (middle * (1.0 + k1 * middle * middle) < distance) low = middle;
          else high = middle;
        }
        float radius = (low + high) * 0.5;
        if (1.0 + 3.0 * k1 * radius * radius <= 0.0) discard;
        vec2 auxiliary = centre + scale * distorted * (distance > 1e-8 ? radius / distance : 1.0);
        if (auxiliary.x < support.x || auxiliary.y < support.y || auxiliary.x > support.z || auxiliary.y > support.w) discard;
        vec2 uv = (auxiliary - support.xy + 0.5) / (support.zw - support.xy + 1.0);
        uv.y = 1.0 - uv.y;
        vec4 pixel = texture2D(image, uv);
        if (pixel.a <= 0.0) discard;
        gl_FragColor = vec4(pixel.rgb / pixel.a, pixel.a);
        #include <colorspace_fragment>
        gl_FragColor.rgb *= gl_FragColor.a;
      }
    `,
  });
  private readonly quad = new THREE.Mesh(new THREE.PlaneGeometry(2, 2), this.material);

  constructor() {
    this.quad.frustumCulled = false;
    this.scene.add(this.quad);
  }

  render(renderer: THREE.WebGLRenderer, scene: THREE.Scene, camera: THREE.PerspectiveCamera, shot: ResultShot, frame: ResultFrame, cloudBounds?: THREE.Box3) {
    const projection = shot.projection;
    if (!projection || !frame.postwarp) throw new Error('本帧缺少完整的径向投影或 postwarp，已停止叠加。');
    const { radial } = projection;
    const support = cloudBounds ? extrapolatedSupport(projection, cloudBounds) : radial.support;
    const [left, top, right, bottom] = support;
    if (Math.max(right - left + 1, bottom - top + 1) > renderer.capabilities.maxTextureSize) throw new Error('广告投影范围超过当前 GPU 的纹理上限；请缩小广告位，或关闭实验性外推。');
    this.target.setSize(right - left + 1, bottom - top + 1);
    applyAuxiliaryCamera(camera, projection.matrix, shot.centre, support);
    const uniforms = this.material.uniforms;
    const magnitude = Math.max(...frame.postwarp.map(Math.abs));
    const warp = new THREE.Matrix3().fromArray(frame.postwarp.map((x) => x / magnitude)).transpose();
    if (!Number.isFinite(warp.determinant()) || Math.abs(warp.determinant()) < 1e-14) throw new Error('本帧 postwarp 不可逆，已停止叠加。');
    uniforms.inversePostwarp.value.copy(warp.invert());
    uniforms.resolution.value.set(shot.width, shot.height);
    uniforms.centre.value.set(...radial.centre);
    uniforms.support.value.set(...support);
    uniforms.scale.value = radial.scale;
    uniforms.k1.value = radial.k1;
    uniforms.maximumRadius.value = Math.max(...[left, right].flatMap((x) =>
      [top, bottom].map((y) => Math.hypot(x - radial.centre[0], y - radial.centre[1]) / radial.scale)));
    if (cloudBounds && radial.k1 < 0) uniforms.maximumRadius.value = Math.min(uniforms.maximumRadius.value, Math.sqrt(-1 / (3 * radial.k1)) * (1 - 1e-6));
    try {
      renderer.setRenderTarget(this.target);
      renderer.render(scene, camera);
      renderer.setRenderTarget(null);
      renderer.render(this.scene, this.camera);
    } finally { renderer.setRenderTarget(null); }
  }

  dispose() {
    this.target.dispose();
    this.quad.geometry.dispose();
    this.material.dispose();
  }
}
